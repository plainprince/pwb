//! Native System Domains Protocol Handler (`pwb-node::system`).
//!
//! PlainWeb replaces traditional centralized web services with serverless system domains:
//! - `search.plain`: Decentralized search engine API & dynamic interface generation.
//! - `devtools.plain`: Built-in Developer Tools DOM & Style Inspector.
//! - `gossip.plain`: P2P artifact gossip and index propagation.
//! - `nodes.plain`: Peer node routing and health registry.
//! - `register.plain`: Domain registration interface powered by sparse credit ledger.
//! - `network.plain`: Live addresses, peer table, and local-service counts.
//! - `help.plain`: System-domain index with namespace, hosting, and credit rules.
//! - `chat.plain`: Direct end-to-end-encrypted peer chat, plus the public
//!   rooms at `chat.plain?room=<name>`.

mod chat_page;
mod devtools;
mod handler;
mod network_page;
mod registry_page;
mod room_page;
mod search_page;

#[cfg(test)]
mod tests;

pub use crate::chat::ChatSnapshot;
pub use crate::room::RoomSnapshot;
pub use handler::{NetworkSnapshot, SystemDomainHandler};
pub use network_page::{render_help_page_bundle, render_network_page_bundle};
pub use registry_page::{render_message_bundle, render_register_page_bundle};
pub use search_page::{render_search_pending_bundle, render_search_results_bundle};
