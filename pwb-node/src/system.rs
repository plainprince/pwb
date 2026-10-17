//! Native System Domains Protocol Handler (`pwb-node::system`).
//!
//! PlainWeb replaces traditional centralized web services with serverless system domains:
//! - `search.plain`: Decentralized search engine API & dynamic interface generation.
//! - `devtools.plain`: Built-in Developer Tools DOM & Style Inspector.
//! - `gossip.plain`: P2P artifact gossip and index propagation.
//! - `nodes.plain`: Peer node routing and health registry.
//! - `register.plain`: Domain registration interface powered by sparse credit ledger.

mod devtools;
mod handler;
mod registry_page;
mod search_page;

#[cfg(test)]
mod tests;

pub use handler::SystemDomainHandler;
