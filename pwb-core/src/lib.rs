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
//!   components  — reusable UI component primitives (sidebars, navbars, tabs)
//!                 shared by both `pwb-web` and `pwb-browser`.

pub mod components;
pub mod crypto;
pub mod net;
pub mod spec;
pub mod wire;
