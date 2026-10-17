//! Network framing and system domain protocol message types (pwb-core::net).
//!
//! PlainWeb transport is serverless. System domains (search.plain, gossip.plain,
//! nodes.plain, register.plain) provide native network services directly without
//! central servers.
//!
//! This module provides the NodeEngine which consolidates peer table management,
//! routing logic, framing serialization, and socket event loops. Both pwb-node
//! and pwb-browser instantiate this engine to ensure they run on the exact same
//! network stack.
//!
//! [`CellChannel`] carries onion cells hop-by-hop over UDP with source-IP
//! masking (TODO §2.3): each relay re-originates the datagram from its own
//! socket, so a receiving hop only ever observes the previous relay's
//! address.

mod cell_channel;
mod engine;
mod loops;
mod messages;

#[cfg(test)]
mod tests;

pub use cell_channel::CellChannel;
pub use engine::{NodeConfig, NodeEngine, PeerInfo};
pub use messages::{NetworkError, NetworkMessage, SearchResultWire, Transport};
