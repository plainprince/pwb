//! `pwb-node` — PlainWeb layer 3: the hoster + miner.
//!
//! A node is a participant in a fully serverless network. It hosts sites,
//! indexes and ranks search queries, verifies peers' work, manages identity,
//! and handles system domain protocols (`search.plain`, `gossip.plain`, `nodes.plain`, `register.plain`).

pub mod domain;
pub mod host;
pub mod identity;
pub mod search;
pub mod system;
pub mod verifier;
