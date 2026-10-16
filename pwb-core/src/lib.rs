//! `pwb-core` — shared primitives for the PlainWeb network.
//!
//! Every other layer builds on this crate. It defines the *shapes* shared by
//! authoring, the runtime, and the network, but implements almost nothing yet.
//!
//! Modules:
//!   spec        — the "plain" markup/style/document-API surface (tag model,
//!                 selector model, document functions).
//!   wire        — the versioned *plain* bundle format + the zip-style compression
//!                 (per-bundle element/selector symbol tables).
//!   crypto      — keypair identity, signatures, and packet encryption.
//!   net         — the transport abstraction (share-links, gossip, broadcast/
//!                 multicast) as framing only.
//!   onion       — fixed-size Sphinx-style onion cells: layered
//!                 ChaCha20-Poly1305 over per-hop X25519 ECDH keys.
//!   system      — the unified system protocol resolver: reserved `.plain`
//!                 domain list, `plain://` URL parsing, and the `nodes.plain`
//!                 connection-link codec.
//!   discovery   — local peer discovery: signed LAN beacons over UDP
//!                 broadcast + IP multicast (§3.1).
//!   qr          — QR code encoding (byte mode v1-10, level M) and pairing
//!                 payload parsing for out-of-band node exchange (§3.3).
//!   dht         — Kademlia routing table: XOR-metric k-buckets with
//!                 ping/pong health checks and latency-weighted contact
//!                 selection (§3).
//!   components  — reusable UI component primitives (sidebars, navbars, tabs)
//!                 shared by both `pwb-web` and `pwb-browser`.

pub mod components;
pub mod crypto;
pub mod dht;
pub mod discovery;
pub mod net;
pub mod onion;
pub mod qr;
pub mod spec;
pub mod system;
pub mod wire;
