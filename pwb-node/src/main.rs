//! `pwb-node` CLI node launcher — runs a live PlainWeb network node.
//!
//! The node binds a UDP socket, starts the [`pwb_core::net::NodeEngine`]
//! (sender loop, receiver loop, peer maintenance, and LAN discovery), and can
//! connect to peers via `plain://nodes.plain?connect=<ip:port>/<pubkey>`
//! links or scanned QR pairing payloads.

use pwb_core::crypto::{ContentId, PublicKey};
use pwb_core::net::{
    peer_exchange_loop, prefer_family, resolve_bootstrap_peer, BootstrapPeer, NetworkMessage,
    NodeConfig, NodeEngine,
};
use pwb_core::onion::CoverConfig;
use pwb_core::wire::Bundle;
use pwb_node::domain::{ClaimError, DomainRegistry, DomainService, PricingEpochStats};
use pwb_node::federation::DistributedSearch;
use pwb_node::health::{HealthServer, HealthState};
use pwb_node::host::ArtifactHost;
use pwb_node::identity::IdentityManager;
use pwb_node::join;
use pwb_node::peers;
use pwb_node::persist;
use pwb_node::search::SearchEngine;
use pwb_node::system::SystemDomainHandler;
use pwb_node::verifier::{IndexProof, PeerVerifier};
use pwb_node::wallet::{self, Wallet};
use pwb_web::parse::parse_site_directory;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often a claim deferred for lack of credits is retried while the node
/// keeps serving. Claims are cheap to attempt and idempotent for the owner.
const CLAIM_RETRY_INTERVAL_SECS: u64 = 15;

/// Parse a 32-byte hex content ID into a [`ContentId`].
fn parse_content_id(hex: &str) -> Option<ContentId> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut cid = [0u8; 32];
    cid.copy_from_slice(&bytes);
    Some(ContentId(cid))
}

/// Parse a 32-byte hex public key.
fn parse_public_key(hex: &str) -> Option<PublicKey> {
    let bytes = hex::decode(hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(PublicKey(key))
}

fn print_help() {
    println!("PlainWeb Node Usage:");
    println!("  pwb-node [options]");
    println!();
    println!("Joining the network:");
    println!("  -c, --connect <link>       Dial a peer via a nodes.plain connection link");
    println!("      --bootstrap <host:port>/<hex_pubkey>  Seed/rendezvous peer (repeatable; or set PWB_BOOTSTRAP)");
    println!("      --join <url|path>      Fetch a well-known join descriptor (repeatable)");
    println!(
        "                              (or set PWB_JOIN; default: deploy/join.json on GitHub)"
    );
    println!("      --no-join              Do not fetch the default join descriptor");
    println!("      --join-required        Exit when no join seed answers, instead of running standalone");
    println!("      --join-signer <hex>    Pin the descriptor signer (or set PWB_JOIN_SIGNER)");
    println!("      --require-join-signature  Reject unsigned join descriptors");
    println!("      --join-seed <host:port>/<hex_pubkey>  Seed for a descriptor you publish (repeatable)");
    println!("      --join-expires <unix>  Expiry timestamp for a descriptor you publish");
    println!("      --write-join <path>    Write a signed join descriptor and exit");
    println!("                              (uses --join-seed, --trust-anchor, --write-join-key)");
    println!(
        "      --write-join-key <path>  Identity used to sign --write-join (default: this node's)"
    );
    println!("      --verify-join <url|path>  Check a descriptor's signature, expiry, and seeds, then exit");
    println!(
        "      --withdraw-join       With --write-join: publish a signed zero-seed withdrawal"
    );
    println!(
        "      --trust-anchor <hex>   Pin funding attestors (repeatable; or set PWB_TRUST_ANCHORS)"
    );
    println!("      --stun-server <host:port>  STUN server (default: stun.l.google.com:19302)");
    println!("      --no-stun              Do not contact STUN (LAN-only reachability)");
    println!("      --listen <addr>        UDP listen address (default: 0.0.0.0:0; use [::]:0 for dual-stack IPv4+IPv6)");
    println!(
        "      --identity <path>      Persisted identity file (default: $HOME/.pwb/identity.json)"
    );
    println!("      --data <dir>           Data directory (default: $HOME/.pwb/data)");
    println!("                              (home dir: $HOME, else %USERPROFILE% on Windows)");
    println!();
    println!("Serving and searching:");
    println!("      --host <dir|file>      Publish a site directory or .pwb bundle");
    println!("      --search <query>       Run one federated query over the live swarm and exit");
    println!("      --private-search <query>  Run one query over onion circuits and exit");
    println!(
        "      --chat <peer_hex>       Open an E2EE chat REPL with a peer (runs with the daemon)"
    );
    println!("      --chat-via <route>      REPL send route: direct (default), onion, or auto");
    println!("      --room <name>           Join a public room and open a REPL to post in it");
    println!("      --demo                 Run the synthetic workload instead of real work");
    println!();
    println!("Domains and credits:");
    println!("      --claim <name>         Claim a .pwb name for the content you host");
    println!(
        "      --content <hex>        Content ID the claim points at (default: the hosted bundle)"
    );
    println!("      --attested-claim <name>   Claim against a k-of-N attested balance");
    println!("      --blind-claim <name>      Claim privately with a stored bearer voucher");
    println!("      --withdraw <amount>    Convert attested credits into a blind voucher");
    println!("      --issuer <hex>         Restrict --withdraw to one attestor");
    println!(
        "      --voucher-file <path>  Voucher wallet location (default: <data>/vouchers.json)"
    );
    println!("      --domains <path>       Domain registry file (default: <data>/domains.json)");
    println!("      --status               Print identity, credits, attested balance, and wallet, then exit");
    println!();
    println!("Operations:");
    println!(
        "      --health-addr <addr>   Loopback health/metrics endpoint (default: 127.0.0.1:7711)"
    );
    println!("      --no-health            Disable the health endpoint");
    println!("      --cover-traffic        Emit decoy onion cells at randomized intervals (costs bandwidth)");
    println!("  -h, --help                 Show this help message");
}

#[tokio::main]
async fn main() {
    // Parse CLI arguments (Connection Links & URI Handlers).
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }
    let mut arg_index = 1;
    let mut connect_target: Option<String> = None;
    let mut stun_server: Option<String> = None;
    let mut no_stun = false;
    let mut bootstrap_peers: Vec<BootstrapPeer> = Vec::new();
    // Raw `--bootstrap` specs, resolved once the listen address (and thus
    // the preferred address family) is known.
    let mut bootstrap_specs: Vec<String> = Vec::new();
    let mut trust_anchors: Vec<PublicKey> = Vec::new();
    let mut join_sources: Vec<String> = Vec::new();
    let mut join_seeds: Vec<pwb_core::join::JoinSeed> = Vec::new();
    let mut join_signer: Option<PublicKey> = None;
    let mut join_signing_key: Option<PathBuf> = None;
    let mut join_expires: Option<u64> = None;
    let mut write_join: Option<PathBuf> = None;
    let mut verify_join: Option<String> = None;
    let mut withdraw_join = false;
    let mut no_join = false;
    let mut require_join = false;
    let mut require_join_signature = false;
    let mut cover_traffic = false;
    let mut claim_domain: Option<String> = None;
    let mut claim_content: Option<ContentId> = None;
    let mut identity_path: Option<PathBuf> = None;
    let mut domains_path: Option<PathBuf> = None;
    let mut listen_arg: Option<String> = None;
    let mut host_path: Option<PathBuf> = None;
    let mut data_arg: Option<PathBuf> = None;
    let mut demo_mode = false;
    let mut search_query: Option<String> = None;
    let mut private_search_query: Option<String> = None;
    let mut chat_peer_arg: Option<String> = None;
    let mut chat_via_arg: Option<String> = None;
    let mut room_arg: Option<String> = None;
    let mut attested_claim: Option<String> = None;
    let mut blind_claim: Option<String> = None;
    let mut withdraw_amount: Option<u64> = None;
    let mut voucher_file: Option<PathBuf> = None;
    let mut issuer_key: Option<PublicKey> = None;
    let mut show_status = false;
    let mut health_addr: Option<String> = None;
    let mut no_health = false;
    while arg_index < args.len() {
        match args[arg_index].as_str() {
            "-c" | "--connect" => {
                if arg_index + 1 < args.len() {
                    connect_target = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --connect requires a link argument");
                    std::process::exit(1);
                }
            }
            "--stun-server" => {
                if arg_index + 1 < args.len() {
                    stun_server = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --stun-server requires a host:port argument");
                    std::process::exit(1);
                }
            }
            "--no-stun" => {
                no_stun = true;
                arg_index += 1;
            }
            "--bootstrap" => {
                if arg_index + 1 < args.len() {
                    bootstrap_specs.push(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --bootstrap requires a peer argument");
                    std::process::exit(1);
                }
            }
            "--join" => {
                if arg_index + 1 < args.len() {
                    join_sources.push(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --join requires a URL, file path, or inline JSON");
                    std::process::exit(1);
                }
            }
            "--no-join" => {
                no_join = true;
                arg_index += 1;
            }
            "--join-required" => {
                require_join = true;
                arg_index += 1;
            }
            "--require-join-signature" => {
                require_join_signature = true;
                arg_index += 1;
            }
            "--join-signer" => {
                if arg_index + 1 < args.len() {
                    match parse_public_key(&args[arg_index + 1]) {
                        Some(key) => join_signer = Some(key),
                        None => {
                            eprintln!("Error: --join-signer requires a 32-byte hex public key");
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --join-signer requires a public key");
                    std::process::exit(1);
                }
            }
            "--join-seed" => {
                if arg_index + 1 < args.len() {
                    match join::parse_seed_spec(&args[arg_index + 1]) {
                        Some(seed) => join_seeds.push(seed),
                        None => {
                            eprintln!(
                                "Error: --join-seed requires <host:port>/<hex_pubkey>, got {}",
                                args[arg_index + 1]
                            );
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --join-seed requires a seed argument");
                    std::process::exit(1);
                }
            }
            "--join-expires" => {
                if arg_index + 1 < args.len() {
                    match args[arg_index + 1].parse::<u64>() {
                        Ok(epoch) if epoch > 0 => join_expires = Some(epoch),
                        _ => {
                            eprintln!("Error: --join-expires requires a positive unix timestamp");
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --join-expires requires a unix timestamp");
                    std::process::exit(1);
                }
            }
            "--write-join" => {
                if arg_index + 1 < args.len() {
                    write_join = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --write-join requires a path");
                    std::process::exit(1);
                }
            }
            "--verify-join" => {
                if arg_index + 1 < args.len() {
                    verify_join = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --verify-join requires a URL, file path, or inline JSON");
                    std::process::exit(1);
                }
            }
            "--withdraw-join" => {
                withdraw_join = true;
                arg_index += 1;
            }
            "--write-join-key" => {
                if arg_index + 1 < args.len() {
                    join_signing_key = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --write-join-key requires an identity file path");
                    std::process::exit(1);
                }
            }
            "--cover-traffic" => {
                cover_traffic = true;
                arg_index += 1;
            }
            "--trust-anchor" => {
                if arg_index + 1 < args.len() {
                    match parse_public_key(&args[arg_index + 1]) {
                        Some(key) => trust_anchors.push(key),
                        None => {
                            eprintln!(
                                "Error: --trust-anchor requires a 32-byte hex attestor public key"
                            );
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --trust-anchor requires a public key");
                    std::process::exit(1);
                }
            }
            "--claim" => {
                if arg_index + 1 < args.len() {
                    claim_domain = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --claim requires a domain name argument (e.g. mysite.pwb)");
                    std::process::exit(1);
                }
            }
            "--content" => {
                if arg_index + 1 < args.len() {
                    match parse_content_id(&args[arg_index + 1]) {
                        Some(cid) => claim_content = Some(cid),
                        None => {
                            eprintln!("Error: --content requires a 32-byte hex content ID");
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --content requires a hex content ID argument");
                    std::process::exit(1);
                }
            }
            "--identity" => {
                if arg_index + 1 < args.len() {
                    identity_path = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --identity requires a path argument");
                    std::process::exit(1);
                }
            }
            "--domains" => {
                if arg_index + 1 < args.len() {
                    domains_path = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --domains requires a path argument");
                    std::process::exit(1);
                }
            }
            "--listen" => {
                if arg_index + 1 < args.len() {
                    listen_arg = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --listen requires a host:port argument");
                    std::process::exit(1);
                }
            }
            "--health-addr" => {
                if arg_index + 1 < args.len() {
                    health_addr = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --health-addr requires a host:port argument");
                    std::process::exit(1);
                }
            }
            "--no-health" => {
                no_health = true;
                arg_index += 1;
            }
            "--host" => {
                if arg_index + 1 < args.len() {
                    host_path = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --host requires a site directory or .pwb file");
                    std::process::exit(1);
                }
            }
            "--data" => {
                if arg_index + 1 < args.len() {
                    data_arg = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --data requires a directory");
                    std::process::exit(1);
                }
            }
            "--demo" => {
                demo_mode = true;
                arg_index += 1;
            }
            "--search" => {
                if arg_index + 1 < args.len() {
                    search_query = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --search requires a query argument");
                    std::process::exit(1);
                }
            }
            "--private-search" => {
                if arg_index + 1 < args.len() {
                    private_search_query = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --private-search requires a query argument");
                    std::process::exit(1);
                }
            }
            "--chat" => {
                if arg_index + 1 < args.len() {
                    chat_peer_arg = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --chat requires a peer public key (64 hex characters)");
                    std::process::exit(1);
                }
            }
            "--chat-via" => {
                if arg_index + 1 < args.len() {
                    chat_via_arg = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --chat-via requires a route (direct, onion, or auto)");
                    std::process::exit(1);
                }
            }
            "--room" => {
                if arg_index + 1 < args.len() {
                    room_arg = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --room requires a room name (up to 32 characters)");
                    std::process::exit(1);
                }
            }
            "--attested-claim" => {
                if arg_index + 1 < args.len() {
                    attested_claim = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --attested-claim requires a domain name");
                    std::process::exit(1);
                }
            }
            "--blind-claim" => {
                if arg_index + 1 < args.len() {
                    blind_claim = Some(args[arg_index + 1].clone());
                    arg_index += 2;
                } else {
                    eprintln!("Error: --blind-claim requires a domain name");
                    std::process::exit(1);
                }
            }
            "--withdraw" => {
                if arg_index + 1 < args.len() {
                    match args[arg_index + 1].parse::<u64>() {
                        Ok(amount) if amount > 0 => withdraw_amount = Some(amount),
                        _ => {
                            eprintln!(
                                "Error: --withdraw requires a positive denomination (1, 5, 10, 25, 50)"
                            );
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --withdraw requires a credit amount");
                    std::process::exit(1);
                }
            }
            "--voucher-file" => {
                if arg_index + 1 < args.len() {
                    voucher_file = Some(PathBuf::from(args[arg_index + 1].clone()));
                    arg_index += 2;
                } else {
                    eprintln!("Error: --voucher-file requires a path");
                    std::process::exit(1);
                }
            }
            "--issuer" => {
                if arg_index + 1 < args.len() {
                    match parse_public_key(&args[arg_index + 1]) {
                        Some(key) => issuer_key = Some(key),
                        None => {
                            eprintln!("Error: --issuer requires a 32-byte hex public key");
                            std::process::exit(1);
                        }
                    }
                    arg_index += 2;
                } else {
                    eprintln!("Error: --issuer requires a public key");
                    std::process::exit(1);
                }
            }
            "--status" => {
                show_status = true;
                arg_index += 1;
            }
            _ => {
                arg_index += 1;
            }
        }
    }

    if withdraw_join && write_join.is_none() {
        eprintln!("Error: --withdraw-join only applies to --write-join <path>");
        std::process::exit(1);
    }

    // Release check: verify a descriptor exactly as a client would, without
    // creating an identity or starting the engine. This is what an operator
    // runs against `deploy/join.json` (or its public URL) before publishing.
    if let Some(source) = verify_join {
        let pinned = join_signer.or_else(|| {
            std::env::var("PWB_JOIN_SIGNER")
                .ok()
                .and_then(|value| parse_public_key(&value))
        });
        match join::fetch_descriptor(&source).await {
            Ok(descriptor) => {
                let now = pwb_core::discovery::unix_time();
                if let Err(error) =
                    join::check_descriptor(&descriptor, pinned, require_join_signature, now)
                {
                    eprintln!("[Join] {source}: {error}");
                    std::process::exit(1);
                }
                println!("[Join] Verified {source}");
                match descriptor.signer {
                    Some(signer) => println!("[Join] Signer: {}", signer.to_hex()),
                    None => {
                        println!("[Join] Signer: unsigned");
                        eprintln!(
                            "[Join] Warning: an unsigned descriptor can be rewritten by its host"
                        );
                    }
                }
                match descriptor.expires_at {
                    Some(expires) => println!(
                        "[Join] Expires: epoch {expires}, in {} day(s)",
                        expires.saturating_sub(now) / 86_400
                    ),
                    None => println!("[Join] Expires: never (rotate manually)"),
                }
                if descriptor.generated_at > now + 3_600 {
                    eprintln!("[Join] Warning: generated_at is more than an hour in the future");
                }
                println!("[Join] Seeds: {}", descriptor.seeds.len());
                for seed in &descriptor.seeds {
                    println!("  {}  {}", seed.address, seed.public_key.to_hex());
                }
                if descriptor.seeds.is_empty() {
                    if descriptor.signer.is_some() {
                        println!("[Join] Status: signed withdrawal (zero seeds)");
                    } else {
                        eprintln!(
                            "[Join] Warning: no seeds are published; clients will fall back to LAN discovery"
                        );
                    }
                }
                println!("[Join] Trust anchors: {}", descriptor.trust_anchors.len());
                for anchor in &descriptor.trust_anchors {
                    println!("  {}", anchor.to_hex());
                }
                return;
            }
            Err(error) => {
                eprintln!("[Join] {source}: {error}");
                std::process::exit(1);
            }
        }
    }

    let data_dir = data_arg
        .or_else(|| std::env::var_os("PWB_DATA").map(PathBuf::from))
        .or_else(|| pwb_core::paths::home_dir().map(|home| home.join(".pwb").join("data")))
        .unwrap_or_else(|| PathBuf::from("pwb-data"));

    // `--chat <hex>` pairs the stdin REPL below with one peer identity.
    let chat_peer = chat_peer_arg.as_deref().map(|hex| {
        parse_public_key(hex).unwrap_or_else(|| {
            eprintln!("Error: --chat requires a 64-hex-character peer public key");
            std::process::exit(1);
        })
    });

    // `--chat-via` picks how REPL lines travel: sealed straight to the peer,
    // across a three-hop circuit, or a circuit when one can be built (with a
    // visible note when it falls back to revealing both addresses).
    let chat_via = chat_via_arg.as_deref().map(str::to_ascii_lowercase);
    let chat_route = match chat_via.as_deref() {
        None | Some("direct") => pwb_node::chat::ChatRoute::Direct,
        Some("onion") => pwb_node::chat::ChatRoute::Onion,
        Some("auto") => pwb_node::chat::ChatRoute::Auto,
        Some(other) => {
            eprintln!("Error: --chat-via must be direct, onion, or auto (got {other})");
            std::process::exit(1);
        }
    };

    // `--room <name>` opens a second REPL, this one posting to a public
    // room. `--chat` and `--room` both own stdin, so run one at a time.
    let room_arg = room_arg.map(|name| {
        let normalized = name.trim().to_lowercase();
        if !pwb_core::net::is_valid_room_name(&normalized) {
            eprintln!("Error: --room needs up to 32 letters, digits, - _ . (got {name:?})");
            std::process::exit(1);
        }
        normalized
    });
    if room_arg.is_some() && chat_peer.is_some() {
        eprintln!("Error: --chat and --room each open a REPL; run one at a time");
        std::process::exit(1);
    }

    let identity_path = identity_path
        .or_else(IdentityManager::default_identity_path)
        .expect("no identity path available (set PWB_IDENTITY, or a home directory via $HOME / %USERPROFILE%)");
    let identity = IdentityManager::load_or_create(&identity_path);
    println!("  Node Identity Public Key : {}", identity.public_key());

    // The listen address decides which address family this node can dial, so
    // it is parsed before anything resolves a remote host: bootstrap seeds
    // and the STUN server are both picked to match it, so an IPv6-only node
    // is never handed an IPv4 address it can never reach (and vice versa).
    let listen_addr: SocketAddr = listen_arg
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or("0.0.0.0:0".parse().unwrap());
    let preferred_family = Some(listen_addr.ip());
    for spec in &bootstrap_specs {
        match resolve_bootstrap_peer(spec, preferred_family).await {
            Some(peer) => bootstrap_peers.push(peer),
            None => {
                eprintln!("Error: --bootstrap requires <host:port>/<hex_pubkey>, got {spec}");
                std::process::exit(1);
            }
        }
    }

    // --- Network stack: start the NodeEngine with live UDP transport -------
    //
    // This binds a UDP socket, spawns the sender/receiver loops, the peer
    // maintenance loop, and a DiscoveryService that shares the engine's peer
    // table. LAN beacons (broadcast + multicast on port 7922) will populate
    // the peer table automatically, and the engine can send/receive
    // NetworkMessage datagrams to/from any registered peer.
    //
    // STUN discovers this node's public IP:port through NAT so connection
    // links are dialable from the internet. Defaults to Google's public STUN
    // server so a node behind a router is reachable without any flags. The
    // server may be a hostname (`host:port`), so resolve it to an IP socket
    // address here; a resolution failure is non-fatal (STUN is skipped).
    let default_stun_server = "stun.l.google.com:19302";
    let stun_host = stun_server.as_deref().unwrap_or(default_stun_server);
    let effective_stun_server = if no_stun {
        None
    } else {
        match tokio::net::lookup_host(stun_host).await {
            // Query a STUN server of the family this node's socket actually
            // dials: a v4-only socket cannot reach an AAAA-only server. Falls
            // back to any family when none match the preference.
            Ok(addrs) => prefer_family(addrs, preferred_family),
            Err(e) => {
                eprintln!("[Network] Could not resolve STUN server '{stun_host}': {e}");
                None
            }
        }
    };

    // Bootstrap peers come from `--bootstrap` or the comma-separated
    // `PWB_BOOTSTRAP` environment variable. There is deliberately no baked-in
    // localhost placeholder: a fake default made standalone nodes look joined
    // and made the two binaries disagree about the same peer key.
    let mut env_bootstrap: Vec<BootstrapPeer> = Vec::new();
    if let Ok(value) = std::env::var("PWB_BOOTSTRAP") {
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            match resolve_bootstrap_peer(entry, preferred_family).await {
                Some(peer) => env_bootstrap.push(peer),
                None => eprintln!("[Network] Ignoring unresolvable bootstrap entry '{entry}'"),
            }
        }
    }
    let explicit_bootstrap = !bootstrap_peers.is_empty() || !env_bootstrap.is_empty();
    let mut effective_bootstrap = if bootstrap_peers.is_empty() {
        env_bootstrap
    } else {
        bootstrap_peers
    };
    // Pinned attestors may also come from a comma-separated environment
    // variable, which is easier to manage in a service unit than repeating
    // the flag. Invalid entries are reported and skipped; a node with no
    // valid anchors keeps the historical live-peer verification.
    if let Ok(value) = std::env::var("PWB_TRUST_ANCHORS") {
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            match parse_public_key(entry) {
                Some(key) => trust_anchors.push(key),
                None => eprintln!("[Trust] Ignoring invalid trust anchor '{entry}'"),
            }
        }
    }
    if let Ok(value) = std::env::var("PWB_JOIN") {
        join_sources.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_string),
        );
    }
    if let Ok(value) = std::env::var("PWB_JOIN_SIGNER") {
        match parse_public_key(&value) {
            Some(key) => join_signer = Some(key),
            None => eprintln!("[Join] Ignoring invalid PWB_JOIN_SIGNER"),
        }
    }
    trust_anchors.sort_unstable_by_key(|key| key.0);
    trust_anchors.dedup();

    // Publish a descriptor from this operator's seed/anchor list. This is a
    // maintenance command: it never starts the engine or writes a secret.
    if let Some(path) = write_join {
        // A withdrawal is the only case where an empty seed list is
        // intentional. It ignores bootstrap configuration entirely, so a
        // stray PWB_BOOTSTRAP cannot turn a withdrawal into a seed list.
        let seeds = if withdraw_join {
            if !join_seeds.is_empty() {
                eprintln!("Error: --withdraw-join cannot be combined with --join-seed");
                std::process::exit(1);
            }
            Vec::new()
        } else if join_seeds.is_empty() {
            effective_bootstrap
                .iter()
                .map(|peer| pwb_core::join::JoinSeed {
                    address: peer.address.to_string(),
                    public_key: peer.public_key,
                })
                .collect::<Vec<_>>()
        } else {
            join_seeds.clone()
        };
        if seeds.is_empty() && !withdraw_join {
            eprintln!(
                "Error: --write-join needs at least one seed; pass --join-seed <host:port>/<key>"
            );
            std::process::exit(1);
        }
        if let Some(expires) = join_expires {
            if expires <= pwb_core::discovery::unix_time() {
                eprintln!(
                    "Error: --join-expires {expires} is in the past; clients would reject the descriptor"
                );
                std::process::exit(1);
            }
        }
        let signer = match &join_signing_key {
            Some(path) => match IdentityManager::load(path) {
                Some(identity) => identity.keypair,
                None => {
                    eprintln!(
                        "Error: cannot load join signing identity '{}'",
                        path.display()
                    );
                    std::process::exit(1);
                }
            },
            None => identity.keypair.clone(),
        };
        let descriptor =
            join::build_descriptor(seeds, trust_anchors.clone(), join_expires, Some(&signer));
        if let Err(error) = join::write_descriptor(&path, &descriptor) {
            eprintln!("Error: cannot write '{}': {error}", path.display());
            std::process::exit(1);
        }
        if withdraw_join {
            println!(
                "[Join] Wrote a signed withdrawal (no seeds) to {}",
                path.display()
            );
            println!(
                "[Join] Clients with --join-required will exit until a normal descriptor is published again."
            );
        } else {
            println!(
                "[Join] Wrote {} seed(s) to {}",
                descriptor.seeds.len(),
                path.display()
            );
        }
        println!("[Join] Signer public key: {}", signer.public.to_hex());
        println!(
            "[Join] Publish this file at a stable HTTPS URL and pin that URL with --join/PWB_JOIN."
        );
        return;
    }

    // Past the maintenance commands: only a real run starts the engine.
    println!("PlainWeb Layer 3 Serverless Node (pwb-node)");
    println!("Starting node process...");

    // A stranger with no connection link gets the project's well-known join
    // descriptor. Explicit bootstrap entries suppress the network fetch so a
    // LAN or private deployment never depends on GitHub availability.
    // `--no-join` only suppresses the implicit default; an explicit --join /
    // PWB_JOIN source is still honoured.
    if !no_join && join_sources.is_empty() && !explicit_bootstrap {
        join_sources.push(join::DEFAULT_JOIN_URL.to_string());
    }
    if !join_sources.is_empty() {
        println!("[Join] Fetching {} source(s)…", join_sources.len());
        match join::discover(
            &join_sources,
            Some(&data_dir),
            join_signer,
            require_join_signature,
        )
        .await
        {
            Ok(descriptor) => {
                let seeds = join::resolve_seeds(&descriptor, preferred_family).await;
                for peer in seeds {
                    if !effective_bootstrap
                        .iter()
                        .any(|have| have.public_key == peer.public_key)
                    {
                        effective_bootstrap.push(peer);
                    }
                }
                if trust_anchors.is_empty() && !descriptor.trust_anchors.is_empty() {
                    trust_anchors = descriptor.trust_anchors.clone();
                    trust_anchors.sort_unstable_by_key(|key| key.0);
                    trust_anchors.dedup();
                    println!(
                        "[Trust] Using {} anchor(s) from the join descriptor",
                        trust_anchors.len()
                    );
                }
                println!(
                    "[Join] Descriptor published {} seed(s), {} anchor(s)",
                    descriptor.seeds.len(),
                    descriptor.trust_anchors.len()
                );
                if descriptor.seeds.is_empty() {
                    let withdrawn = descriptor.signer.is_some();
                    if require_join {
                        if withdrawn {
                            eprintln!("Error: the published descriptor withdraws all seeds");
                        } else {
                            eprintln!("Error: the join descriptor lists no seeds");
                        }
                        std::process::exit(1);
                    }
                    if withdrawn {
                        eprintln!(
                            "[Join] The published descriptor withdraws all seeds; falling back to LAN discovery."
                        );
                    } else {
                        eprintln!(
                            "[Join] No public seed is published yet; falling back to LAN discovery."
                        );
                    }
                }
            }
            Err(error) => {
                if require_join {
                    eprintln!("Error: no usable join source: {error}");
                    std::process::exit(1);
                }
                eprintln!("[Join] {error}");
                eprintln!("[Join] {}", error.advice());
            }
        }
    }
    // A loopback-only node has no NAT to traverse; skipping STUN keeps local
    // test clusters from waiting on a public server during startup. Operators
    // who do not want their public IP disclosed can pass `--no-stun`.
    let effective_stun_server = if no_stun || listen_addr.ip().is_loopback() {
        None
    } else {
        effective_stun_server
    };
    println!("  Listening on             : {}", listen_addr);
    let config = NodeConfig {
        listen_addr,
        keypair: identity.keypair.clone(),
        bootstrap_peers: effective_bootstrap.clone(),
        stun_server: effective_stun_server,
        max_peers: 100,
        heartbeat_interval: Duration::from_secs(30),
    };
    let engine = NodeEngine::new(config);
    if cover_traffic {
        engine.enable_cover_traffic(CoverConfig::default());
    }
    engine.start().await.expect("node engine start");
    println!("  Private Lookups          : onion circuits (3 hops) for --private-search");
    if let Some(cover) = engine.cover_traffic() {
        println!(
            "  Cover Traffic            : every {:?}-{:?} (mean {:?})",
            cover.min_interval, cover.max_interval, cover.mean_interval
        );
    }

    // The domain registry is seeded from persisted state and gossiped with
    // peers so claims made here are resolvable across the swarm.
    let registry = Arc::new(Mutex::new(DomainRegistry::new()));
    // Registry and signed credit ledger live beside the rest of the node's
    // data, so `--data` gives each node (or test cluster member) its own
    // namespace and balance. `--domains` still overrides the location.
    let domain_persist = domains_path.unwrap_or_else(|| data_dir.join("domains.json"));
    let domain = Arc::new(DomainService::new_with_trust(
        engine.clone(),
        Arc::clone(&registry),
        identity.keypair.clone(),
        0,
        Some(domain_persist),
        trust_anchors,
    ));
    let domain_handle = tokio::spawn(Arc::clone(&domain).run());
    match domain.trust_quorum() {
        Some((required, total)) => {
            println!("  Funding Trust            : {required}-of-{total} pinned attestor(s)")
        }
        None => println!(
            "  Funding Trust            : local peer view (pass --trust-anchor to pin attestors)"
        ),
    }

    // Peer-attested balances, blind voucher issuance, and remote proof
    // verification. Issuer keys are derived from the identity secret (the
    // attestor's blind-signing secret is separate from its Ed25519 key) and
    // rotate per denomination.
    let attestation = Arc::new(pwb_node::attestation::AttestationService::open(
        engine.clone(),
        identity.keypair.clone(),
        &identity.keypair.secret.0,
        data_dir.join("attestations.json"),
    ));
    let attestation_handle = tokio::spawn(Arc::clone(&attestation).run());

    // Credit onion cells this node relays into the same signed ledger used by
    // served queries, indexed bundles, and domain claims.
    let relay_credits = Arc::new(pwb_node::relay::RelayCreditService::with_defaults(
        engine.clone(),
        &identity.keypair,
        Arc::clone(domain.ledger()),
    ));
    let relay_handle = tokio::spawn(Arc::clone(&relay_credits).run(identity.keypair.clone()));

    println!(
        "  Credit Ledger Balance    : {} credits (signed entry chain)",
        domain.available_credits()
    );

    // --- Shared data services ---------------------------------------------
    //
    // Hosted artifacts and the search index are shared by the federated
    // search service (shard queries, postings, artifact fetch) and the system
    // domain responder (search.plain / FetchArtifact). Both persist under the
    // data directory so a node can serve yesterday's content after a restart.
    let artifacts_dir = data_dir.join("artifacts");
    let host = match ArtifactHost::open(&artifacts_dir) {
        Ok(host) => {
            println!(
                "  Artifact Store           : {} ({} artifacts)",
                artifacts_dir.display(),
                host.len()
            );
            Arc::new(host)
        }
        Err(e) => {
            eprintln!(
                "[Host] Cannot open '{}': {}; using an in-memory store",
                artifacts_dir.display(),
                e
            );
            Arc::new(ArtifactHost::new())
        }
    };
    let index_path = data_dir.join("index.json");
    let search = Arc::new(SearchEngine::open(&index_path));
    println!("  Search Index             : {}", index_path.display());

    let federation = Arc::new(
        DistributedSearch::new(
            engine.clone(),
            Arc::clone(&search),
            Arc::clone(&host),
            identity.keypair.clone(),
        )
        .with_ledger(Arc::clone(domain.ledger())),
    );
    let federation_handle = tokio::spawn(Arc::clone(&federation).run());

    // Health/readiness/metrics endpoint. Loopback by default so a public node
    // does not expose an operational surface without opting in.
    let health_server = if no_health {
        None
    } else {
        let addr: SocketAddr = health_addr
            .as_deref()
            .unwrap_or("127.0.0.1:7711")
            .parse()
            .unwrap_or_else(|e| {
                eprintln!("[Health] Invalid --health-addr: {e}");
                std::process::exit(1);
            });
        match HealthServer::spawn(
            addr,
            Arc::new(HealthState {
                engine: engine.clone(),
                search: Arc::clone(&search),
                host: Arc::clone(&host),
                registry: Arc::clone(&registry),
                ledger: Arc::clone(domain.ledger()),
                started: std::time::Instant::now(),
            }),
        ) {
            Ok(server) => {
                println!(
                    "  Health / Metrics         : http://{}/health, /status, /metrics",
                    server.local_addr()
                );
                Some(server)
            }
            Err(e) => {
                eprintln!("[Health] Cannot bind {addr}: {e}");
                None
            }
        }
    };
    pwb_core::telemetry::log_event(
        "info",
        "node_started",
        serde_json::json!({
            "key": identity.public_key().to_hex(),
            "listen": engine
                .local_addr()
                .and_then(|result| result.ok())
                .map(|addr| addr.to_string()),
            "peers": engine.peer_table().lock().unwrap().len(),
            "health": health_server.as_ref().map(|s| s.local_addr().to_string()),
        }),
    );

    // Answer system-domain requests from peers. `DomainService` handles the
    // control plane (claims, registry sync, resolution); this handles the
    // data plane (search queries and artifact fetches).
    let system_identity = identity.clone();
    let system_ledger = Arc::clone(domain.ledger());
    let system_search = Arc::clone(&search);
    let system_host = Arc::clone(&host);
    let system_registry = Arc::clone(&registry);
    let system_engine = engine.clone();
    let system_handle = tokio::spawn(async move {
        let mut inbound = system_engine.subscribe();
        while let Some((from, message)) = inbound.recv().await {
            if !matches!(
                message,
                NetworkMessage::SearchQuery { .. } | NetworkMessage::FetchArtifact { .. }
            ) {
                continue;
            }
            let registry = system_registry.lock().unwrap();
            let handler = SystemDomainHandler::new(
                &system_search,
                &system_host,
                &registry,
                &system_identity,
                system_ledger.balance(),
            );
            if let Some(response) = handler.handle_message(message) {
                let _ = system_engine.send_to_addr(from, response);
            }
        }
    });

    // `chat.plain`: E2EE chat with this node's identity. The daemon
    // receives, stores, and prints inbound chats; transcripts persist under
    // `--data` (`chat.json`) so conversations survive a restart, and
    // `--chat <peer>` adds a stdin REPL so an operator can answer them. The
    // interactive composer lives on the chat.plain page (rendered by the
    // browser's embedded node).
    let chat = pwb_node::chat::ChatService::with_data_dir(engine.clone(), &data_dir);
    let chat_handle = tokio::spawn(Arc::clone(&chat).run());
    if let Some(peer) = chat_peer {
        println!(
            "[Chat] REPL with {} (via {}) — type a message, Ctrl-D to stop",
            peer.to_hex(),
            match chat_route {
                pwb_node::chat::ChatRoute::Direct => "direct",
                pwb_node::chat::ChatRoute::Onion => "onion",
                pwb_node::chat::ChatRoute::Auto => "auto",
            }
        );
        let chat = Arc::clone(&chat);
        tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // Failures are recorded on the service (and shown by the
                // page); echo them here too, since a headless operator has
                // no page to look at.
                if let Err(error) = chat.send_via(&peer, line, chat_route) {
                    eprintln!("[Chat] {error}");
                }
            }
            println!("[Chat] Chat input closed; the node keeps running until Ctrl+C.");
        });
    }

    // Public rooms (`chat.plain?room=`): every node joins the default room
    // so the square has members to flood through and history to serve, and
    // prints what arrives. `--room <name>` adds a stdin REPL for posting to
    // a room, the headless counterpart to the browser's composer.
    let rooms = pwb_node::room::RoomService::with_data_dir(
        engine.clone(),
        identity.keypair.clone(),
        &data_dir,
    );
    let room_handle = tokio::spawn(Arc::clone(&rooms).run());
    if let Err(error) = rooms.join(pwb_node::room::DEFAULT_ROOM) {
        eprintln!("[Room] {error}");
    }
    if let Some(room) = room_arg.as_deref() {
        if let Err(error) = rooms.join(room) {
            eprintln!("[Room] {error}");
        } else {
            println!("[Room] REPL in {room} — type a message, Ctrl-D to stop");
            let rooms = Arc::clone(&rooms);
            let room = room.to_string();
            tokio::task::spawn_blocking(move || {
                use std::io::BufRead;
                for line in std::io::stdin().lock().lines() {
                    let Ok(line) = line else { break };
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    // Failures are recorded on the service (and shown by the
                    // page); echo them here too, since a headless operator
                    // has no page to look at.
                    if let Err(error) = rooms.post(&room, line) {
                        eprintln!("[Room] {error}");
                    }
                }
                println!("[Room] Room input closed; the node keeps running until Ctrl+C.");
            });
        }
    }

    // Answer onion-delivered system requests through their return circuit, so
    // a private search reaches this node without exposing the requester's
    // address. Only data-plane requests are served here; the control plane
    // (claims, registry sync) stays on ordinary authenticated envelopes.
    let onion_identity = identity.clone();
    let onion_ledger = Arc::clone(domain.ledger());
    let onion_search = Arc::clone(&search);
    let onion_host = Arc::clone(&host);
    let onion_registry = Arc::clone(&registry);
    let onion_federation = Arc::clone(&federation);
    let onion_domain = Arc::clone(&domain);
    let onion_engine = engine.clone();
    let onion_handle = tokio::spawn(async move {
        let mut inbound = onion_engine.subscribe_onion_requests();
        while let Some(request) = inbound.recv().await {
            if !request.can_reply() {
                continue;
            }
            let response = match &request.message {
                NetworkMessage::SearchQuery { .. } | NetworkMessage::FetchArtifact { .. } => {
                    let registry = onion_registry.lock().unwrap();
                    let handler = SystemDomainHandler::new(
                        &onion_search,
                        &onion_host,
                        &registry,
                        &onion_identity,
                        onion_ledger.balance(),
                    );
                    handler.handle_message(request.message.clone())
                }
                NetworkMessage::ShardQuery { .. } => {
                    onion_federation.answer_private_shard_query(&request.message, request.sender)
                }
                NetworkMessage::FetchArtifactChunk { .. } => {
                    onion_federation.answer_private_fetch_chunk(&request.message)
                }
                NetworkMessage::DomainQuery { .. } => {
                    onion_domain.answer_private_domain_query(&request.message, request.sender)
                }
                _ => None,
            };
            if let Some(response) = response {
                let _ = onion_engine.send_onion_reply(&request, &response);
            }
        }
    });

    // --- Prepare a site for hosting (`--host <dir|.pwb>`) ------------------
    //
    // Build/validate the bundle before joining, but do not publish it yet:
    // `index_and_publish` replicates postings to the peers it knows, so the
    // node must have joined the swarm first (and announce the artifact after
    // the peer-exchange loop is running).
    let mut hosted_source: Option<(PathBuf, Vec<u8>)> = None;
    if let Some(path) = &host_path {
        let bytes = if path.is_dir() {
            match parse_site_directory(path) {
                Ok(site) => {
                    let bundle = pwb_web::bundle::create_bundle(
                        site.root_node,
                        site.title,
                        None,
                        site.script_payload,
                        site.components,
                        site.modules,
                    );
                    // Publish the same compressed container the `pwb-web` CLI
                    // writes; `Bundle::deserialize` accepts both forms.
                    pwb_web::compress::compress_bundle(&bundle)
                }
                Err(e) => {
                    eprintln!("[Host] Failed to build '{}': {:?}", path.display(), e);
                    std::process::exit(1);
                }
            }
        } else {
            match fs::read(path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    eprintln!("[Host] Failed to read '{}': {}", path.display(), e);
                    std::process::exit(1);
                }
            }
        };

        if Bundle::deserialize(&bytes).is_err() {
            eprintln!("[Host] '{}' is not a valid .pwb bundle", path.display());
            std::process::exit(1);
        }

        hosted_source = Some((path.clone(), bytes));
    }

    let own_addr = engine
        .local_addr()
        .expect("engine socket bound after start")
        .expect("local socket address");
    println!("  Network Listen Address   : {}", own_addr);

    // Report the STUN-discovered public address (NAT traversal) so the
    // operator knows what address peers on the internet will dial.
    if let Some(public_addr) = engine.public_addr() {
        println!("  Network Public Address   : {}  (via STUN)", public_addr);
    } else if effective_stun_server.is_some() {
        println!("  Network Public Address   : (STUN query failed; using local address)");
    }

    if let Some(discovery) = engine.discovery_addr() {
        println!(
            "  LAN Discovery           : {} (broadcast + multicast)",
            discovery
        );
    }

    // Re-dial the swarm this node knew before it restarted. This makes a node
    // behind a NAT, or one whose bootstrap has gone away, rejoin on its own
    // instead of starting from an empty peer table.
    let peer_store_path = data_dir.join("peers.json");
    let reconnected = peers::restore(&engine, &peer_store_path).await;
    if reconnected > 0 {
        println!(
            "[Network] Reconnected to {} remembered peer(s).",
            reconnected
        );
    }

    // Join the swarm: dial every configured bootstrap/rendezvous peer and
    // keep only the ones that answered a liveness probe. Bootstrap nodes
    // share their peer tables via PeerExchange gossip, so a new node learns
    // about the rest of the network automatically.
    let connected = engine.connect_bootstrap_verified().await;
    if !connected.is_empty() {
        println!("[Network] Joined via {} bootstrap peer(s)", connected.len());
        for peer in &connected {
            println!(
                "    -> {} at {}",
                &peer.public_key.to_hex()[..8],
                peer.address
            );
        }
    } else if !effective_bootstrap.is_empty() {
        println!("[Network] No bootstrap peers reachable; running standalone.");
    } else {
        println!("[Network] No bootstrap peers configured; LAN discovery only.");
    }
    if require_join && connected.is_empty() && reconnected == 0 {
        eprintln!(
            "Error: --join-required was set, but no join seed or remembered peer answered a liveness probe"
        );
        std::process::exit(1);
    }

    // Background task: periodically gossip the peer table as PeerExchange so
    // bootstrap/rendezvous nodes advertise known peers and the swarm stays
    // discoverable. Aborted on shutdown below.
    let px_handle = tokio::spawn(peer_exchange_loop(
        engine.clone(),
        identity.public_key(),
        Duration::from_secs(30),
    ));

    // Background task: remember the live peer table so the next process can
    // rejoin without a bootstrap (and while all services are still running).
    let peers_handle = tokio::spawn(peers::persist_loop(
        engine.clone(),
        peer_store_path.clone(),
        Duration::from_secs(60),
        identity.public_key(),
    ));

    // Publish only after the join attempt: postings are replicated to the
    // peers known at publish time, so publishing earlier would leave the
    // content stranded on this node in anything larger than a tiny cluster.
    let mut hosted_content_id: Option<ContentId> = None;
    if let Some((path, bytes)) = hosted_source {
        match federation.index_and_publish(&bytes) {
            Some(cid) => {
                hosted_content_id = Some(cid);
                println!("\n[Host] Published '{}'", path.display());
                println!("       Content ID: {}", cid);
                println!(
                    "       Postings replicated to responsible peers; announced via gossip.plain."
                );
            }
            None => eprintln!("[Host] Failed to store '{}'", path.display()),
        }
    }

    // Generate the connection link using the public (STUN'd) address when
    // available, falling back to the actual bound address. Peers on the
    // internet can dial this link directly.
    let own_link = engine.generate_connection_link();
    println!("\n[Network] Your connection link:");
    println!("  {}", own_link);

    // Render the link as a scannable QR code (§3.3).
    match pwb_core::qr::QrCode::encode(&own_link) {
        Ok(qr) => {
            println!(
                "\n[Network] Scan-to-pair QR (version {}, mask {}):",
                qr.version(),
                qr.mask()
            );
            println!("{}", qr.to_ascii());
        }
        Err(e) => eprintln!("[Network] QR encode failed: {}", e),
    }

    // A first run that reached nobody should end with what to do next, not
    // just a QR code.
    if connected.is_empty() && reconnected == 0 {
        println!("\n[Network] Not connected to any peer yet. Share the link above, or dial one:");
        println!("  pwb-node --connect \"<their link>\"");
        println!("  pwb-node --host ./mysite        # serve a site once connected");
        println!("  pwb-node --bootstrap <host:port>/<key> --search \"term\"");
        println!("  Guide: docs/join-the-network.md");
    }

    // --- Demo workload (`--demo`) -----------------------------------------
    //
    // Everything below is synthetic: a local demo.pwb, a self-signed index
    // proof, and fabricated credit/pricing/gossip numbers. It is opt-in so a
    // real node only does real work.
    let mut demo_content_id: Option<ContentId> = None;
    if demo_mode {
        let demo_path = "demo.pwb";
        if let Ok(bundle_bytes) = fs::read(demo_path) {
            match host.store(bundle_bytes.clone()) {
                Ok(content_id) => {
                    demo_content_id = Some(content_id);
                    println!("\n[Host] Ingested artifact into local store!");
                    println!("       Content ID: {}", content_id);

                    if let Ok(bundle) = Bundle::deserialize(&bundle_bytes) {
                        search.index_bundle(content_id, &bundle);
                        println!("[Search] Indexed bundle terms into decentralized search engine.");

                        let proof = IndexProof::sign(&identity.keypair, content_id, content_id);
                        let is_valid = PeerVerifier::verify_indexing_proof(&proof);
                        println!(
                            "[Verifier] Peer verification check: {}",
                            if is_valid { "PASSED" } else { "FAILED" }
                        );

                        // Credits are not fabricated by the demo: they come
                        // from verified service traffic (see the credit
                        // ledger), so a fresh node starts at zero.
                    }
                }
                Err(e) => eprintln!("Failed to ingest artifact: {}", e),
            }
        } else {
            println!("Note: demo.pwb not found. Run `cargo run -p pwb-web` to generate it.");
        }

        let query = "PlainWeb";
        println!("\n[Search Query] Searching index for '{}':", query);
        let results = search.search(query);
        if results.is_empty() {
            println!("  No search results found.");
        } else {
            for (idx, res) in results.iter().enumerate() {
                println!(
                    "  #{}: \"{}\" | CID: {} | Rank Score: {:.2}",
                    idx + 1,
                    res.title,
                    res.content_id,
                    res.rank_score
                );
            }
        }

        let mut cluster_a = DomainRegistry::new();
        let mut cluster_b = DomainRegistry::new();
        let alice = PublicKey([11; 32]);
        let bob = PublicKey([22; 32]);
        let cid = ContentId([33; 32]);

        let _ = cluster_a.register("alpha.pwb", cid, alice);
        let _ = cluster_b.register("beta.pwb", cid, bob);
        let _ = cluster_a.register("contested.pwb", cid, alice);
        let _ = cluster_b.register("contested.pwb", cid, bob);

        cluster_a.record_pricing_epoch(PricingEpochStats {
            epoch: 1,
            registrations: 40,
            network_work: 1_000,
            node_count: 1,
        });
        cluster_b.record_pricing_epoch(PricingEpochStats {
            epoch: 1,
            registrations: 0,
            network_work: 100_000,
            node_count: 100,
        });

        let price_before = cluster_a.claim_cost();
        cluster_a.merge(&cluster_b);
        let contested_owner = cluster_a
            .resolve("contested.pwb")
            .map(|record| record.owner)
            .expect("contested.pwb exists after merge");
        println!(
            "[Gossip] merged clusters: {} domains; 'contested.pwb' won by {}",
            cluster_a.count(),
            contested_owner
        );
        println!(
            "[Gossip] merged base cost: {} credits (max of cluster prices {} and {})",
            cluster_a.claim_cost(),
            price_before,
            cluster_b.claim_cost()
        );
    } // end demo workload

    // --- Network: connect to a peer via connection link -------------------
    //
    // Dialing proves the peer actually answers before we call it connected.
    // The liveness probe registers both sides: our signed envelope tells the
    // remote node our key and real source address, and the Pong registers its
    // address here.
    if let Some(target) = connect_target {
        println!(
            "\n[Network] Launching connection to peer via link: {}",
            target
        );
        // Scanned QR payloads may use the compact `<ip:port>/<hex>` form.
        let parsed = NodeEngine::parse_connection_link(&target)
            .or_else(|| pwb_core::qr::parse_pairing_payload(&target));
        match parsed {
            Some((addr, key)) => match engine
                .dial_resilient(addr, key, Duration::from_secs(2))
                .await
            {
                Ok(peer) => {
                    println!(
                        "[Network] Connected to peer {} at {} (reputation 0)",
                        peer.public_key, peer.address
                    );
                    // Announce ourselves so both peer tables stay current.
                    let _ = engine.send_to(
                        &peer.public_key,
                        pwb_core::net::NetworkMessage::PeerDiscovery {
                            known_peers: Vec::new(),
                            sender_pubkey: identity.public_key(),
                        },
                    );
                }
                Err(e) => eprintln!("[Network] Peer never answered: {:?}", e),
            },
            None => eprintln!("[Network] Could not parse connection link"),
        }
    }

    // Explicit domain claim: signed with the persisted identity key and
    // gossiped to the swarm so any node can resolve the name. Runs after the
    // connect/dial step so the claim also reaches the newly joined peer.
    let mut claim_failed = false;
    // A claim refused only because the balance is still short is deferred, not
    // fatal: the node keeps serving (which is what earns the credits) and the
    // background retry below claims the name once the ledger can pay for it.
    let mut deferred_claim: Option<(String, ContentId)> = None;
    if let Some(domain_name) = claim_domain {
        let target = claim_content
            .or(hosted_content_id)
            .or(demo_content_id)
            .expect("no content ID available: pass --content <hex> or --host <dir|file> first");
        println!("\n[Claim] Claiming '{domain_name}' → {target}…");
        match domain.claim(&domain_name, target) {
            Ok(record) => {
                println!(
                    "[Claim] Claimed '{}' → {} (balance now {} credits)",
                    record.domain,
                    record.target_content_id,
                    domain.available_credits()
                );
            }
            Err(ClaimError::InsufficientCredits { balance, cost }) => {
                println!("[Claim] Refused for now: need {cost} credits, have {balance}");
                println!(
                    "[Claim] The node keeps serving and retries this claim every \
                     {CLAIM_RETRY_INTERVAL_SECS}s once the balance covers the price."
                );
                deferred_claim = Some((domain_name, target));
            }
            Err(e) => {
                eprintln!("[Claim] Refused: {e}");
                claim_failed = true;
            }
        }
    }

    // --- Operator wallet: attested claims, withdrawals, blind claims -----
    //
    // These are one-shot operator actions over the live peer set. Attested
    // claims gather a k-of-N balance certificate from the deterministic
    // neighborhood; blind claims redeem a bearer voucher withdrawn earlier.
    // The voucher file holds bearer material, so it is written 0600 on Unix.
    let wallet_path = voucher_file
        .clone()
        .unwrap_or_else(|| Wallet::default_path(&data_dir));
    let mut wallet = Wallet::open(&wallet_path);
    let mut run_daemon = true;
    let mut action_failed = false;
    let claim_target = claim_content.or(hosted_content_id).or(demo_content_id);

    if show_status {
        run_daemon = false;
        println!("\n[Status] Identity          : {}", identity.public_key());
        println!(
            "[Status] Local credits     : {}",
            domain.available_credits()
        );
        print!("[Status] Earned by service : ");
        let earnings = wallet::earnings_by_kind(&domain);
        if earnings.is_empty() {
            println!("none yet");
        } else {
            let rendered: Vec<String> = earnings
                .iter()
                .map(|(kind, amount)| format!("{kind}={amount}"))
                .collect();
            println!("{}", rendered.join(", "));
        }
        match wallet::balance_certificate(&engine, &domain, Duration::from_secs(1)).await {
            Some(certificate) => println!(
                "[Status] Attested balance  : {} credits ({} attestor(s), epoch {})",
                certificate.balance,
                certificate.attestations.len(),
                certificate.epoch
            ),
            None => println!("[Status] Attested balance  : no peer quorum answered"),
        }
        println!(
            "[Status] Voucher wallet    : {} ({} voucher(s), {} credits)",
            wallet.path().display(),
            wallet.vouchers().len(),
            wallet.total()
        );
        for voucher in wallet.vouchers() {
            let (attestor, epoch, denomination) = voucher.issuer();
            println!(
                "           - {denomination} credits from {} (epoch {epoch})",
                wallet::short_key(&attestor)
            );
        }
    }

    if let Some(amount) = withdraw_amount {
        run_daemon = false;
        println!("\n[Wallet] Withdrawing a {amount}-credit blind voucher…");
        match wallet::withdraw_from_peers(
            &engine,
            &domain,
            amount,
            issuer_key,
            Duration::from_secs(2),
        )
        .await
        {
            Ok((receipt, issuer)) => match wallet.add_receipt(&receipt) {
                Ok(added) => println!(
                    "[Wallet] Withdrew {} voucher(s) totalling {} credits from {}; saved to {}",
                    added,
                    receipt.total,
                    wallet::short_key(&issuer),
                    wallet.path().display()
                ),
                Err(e) => {
                    eprintln!("[Wallet] Could not save the vouchers: {e}");
                    action_failed = true;
                }
            },
            Err(e) => {
                eprintln!("[Wallet] Withdrawal failed: {e}");
                action_failed = true;
            }
        }
    }

    if let Some(name) = attested_claim {
        run_daemon = false;
        match claim_target {
            None => {
                eprintln!("[Claim] No content ID: pass --content <hex> or --host <dir|file>");
                action_failed = true;
            }
            Some(target) => {
                println!("\n[Claim] Attested claim of '{name}' → {target}…");
                match wallet::attested_claim(
                    &engine,
                    &domain,
                    &name,
                    target,
                    Duration::from_secs(2),
                )
                .await
                {
                    Ok(record) => println!(
                        "[Claim] Claimed '{}' → {} against an attested balance",
                        record.domain, record.target_content_id
                    ),
                    Err(e) => {
                        eprintln!("[Claim] Attested claim refused: {e}");
                        action_failed = true;
                    }
                }
            }
        }
    }

    if let Some(name) = blind_claim {
        run_daemon = false;
        match claim_target {
            None => {
                eprintln!("[Claim] No content ID: pass --content <hex> or --host <dir|file>");
                action_failed = true;
            }
            Some(target) => {
                println!("\n[Claim] Private (blind) claim of '{name}' → {target}…");
                match wallet::blind_claim(&domain, &mut wallet, &name, target) {
                    Ok(record) => println!(
                        "[Claim] Claimed '{}' → {} with a blind voucher; {} credits left in the wallet",
                        record.domain,
                        record.target_content_id,
                        wallet.total()
                    ),
                    Err(e) => {
                        eprintln!("[Claim] Blind claim refused: {e}");
                        action_failed = true;
                    }
                }
            }
        }
    }

    // Private (onion) search: the query travels through three relays and the
    // exit answers along the return circuit, so the queried node never learns
    // this node's address.
    if let Some(query) = private_search_query {
        run_daemon = false;
        println!("\n[Search] Private (onion) query for '{}'…", query);
        match federation.query_private(&query).await {
            Err(e) => {
                eprintln!("[Search] {e}");
                action_failed = true;
            }
            Ok(results) => {
                if results.is_empty() {
                    println!("  No results from the sharded index.");
                }
                for (idx, result) in results.iter().enumerate() {
                    println!(
                        "  #{}: \"{}\" | CID: {} | Rank: {:.2}",
                        idx + 1,
                        result.title,
                        result.content_id,
                        result.rank_score
                    );
                }
            }
        }
    }

    // One-shot federated query (`--search`): run it after peers are connected,
    // print what the swarm returned, and fetch the top hit to prove artifact
    // serving works end-to-end. The process then exits instead of daemonizing.
    if let Some(query) = search_query {
        run_daemon = false;
        println!("\n[Search] Querying the swarm for '{}'…", query);
        let results = federation.query(&query).await;
        if results.is_empty() {
            println!("  No results from shards.");
        } else {
            for (idx, result) in results.iter().enumerate() {
                println!(
                    "  #{}: \"{}\" | CID: {} | Rank: {:.2}",
                    idx + 1,
                    result.title,
                    result.content_id,
                    result.rank_score
                );
            }
            let top = &results[0];
            match federation.fetch(&top.content_id).await {
                Some(bytes) => println!(
                    "[Fetch] Received {} bytes for {}",
                    bytes.len(),
                    top.content_id
                ),
                None => println!("[Fetch] No artifact response for {}", top.content_id),
            }
        }
    }

    println!(
        "\nNode remaining balance: {} credits.",
        domain.available_credits()
    );

    // Deferred claim retry: serving traffic is what earns the credits, so the
    // process that was told to claim a name must not exit just because the
    // ledger was short at startup.
    let deferred_claim_handle = deferred_claim.map(|(name, target)| {
        let domain = Arc::clone(&domain);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(CLAIM_RETRY_INTERVAL_SECS)).await;
                if domain.available_credits() < domain.claim_cost() {
                    continue;
                }
                match domain.claim(&name, target) {
                    Ok(record) => {
                        println!(
                            "[Claim] Claimed '{}' → {} (balance now {} credits)",
                            record.domain,
                            record.target_content_id,
                            domain.available_credits()
                        );
                        break;
                    }
                    Err(ClaimError::InsufficientCredits { .. }) => continue,
                    Err(e) => {
                        eprintln!("[Claim] Retry failed for '{name}': {e}");
                        break;
                    }
                }
            }
        })
    });

    if run_daemon && !claim_failed {
        println!("Node daemon active. Press Ctrl+C to stop.");

        // Keep the node alive so it can send/receive network traffic.
        // Discovery beacons continue announcing on the LAN; inbound messages
        // are dispatched to the handler loop by the receiver loop.
        tokio::signal::ctrl_c().await.expect("wait for Ctrl+C");
    }

    // Persist the identity so the peer ID survives a restart. The balance
    // lives in the signed credit ledger, not in the identity file.
    let _ = identity.save(&identity_path);
    if let Err(e) = search.flush() {
        eprintln!("[Search] Failed to persist the index: {}", e);
    }
    // Remember the current peers one last time before the socket closes.
    peers_handle.abort();
    if let Err(e) = persist::save_peers(
        &peer_store_path,
        &peers::merged(&engine, &identity.public_key(), &peer_store_path),
    ) {
        eprintln!("[Network] Could not persist peer store: {}", e);
    }
    domain_handle.abort();
    federation_handle.abort();
    system_handle.abort();
    chat_handle.abort();
    room_handle.abort();
    attestation_handle.abort();
    relay_handle.abort();
    onion_handle.abort();
    px_handle.abort();
    if let Some(handle) = deferred_claim_handle {
        handle.abort();
    }
    engine.stop().await;
    println!("[Network] Node stopped.");
    if claim_failed || action_failed {
        std::process::exit(1);
    }
}
