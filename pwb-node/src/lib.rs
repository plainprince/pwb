//! `pwb-node` — PlainWeb layer 3: the hoster + miner.
//!
//! A node is a participant in a fully serverless network. It hosts sites,
//! indexes and ranks search queries, verifies peers' work, manages identity,
//! and handles system domain protocols (`search.plain`, `gossip.plain`, `nodes.plain`, `register.plain`, `chat.plain`).

pub mod attestation;
pub mod blind;
pub mod chat;
pub mod credit;
pub mod domain;
pub mod federation;
pub mod health;
pub mod host;
pub mod identity;
pub mod join;
pub mod ledger;
pub mod peers;
pub mod persist;
pub mod privacy;
pub mod relay;
pub mod room;
pub mod search;
pub mod seed;
pub mod system;
pub mod verifier;
pub mod wallet;
