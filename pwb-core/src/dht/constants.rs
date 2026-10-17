//! Routing-table constants and tunables.

use std::time::Duration;

/// Kademlia bucket capacity (contacts per bucket).
pub const K_BUCKET_SIZE: usize = 8;

/// Node-ID width in bits.
pub const ID_BITS: usize = 256;

/// Consecutive-failure threshold after which an entry is evicted.
pub const MAX_FAILURES: u32 = 3;

/// Quiet-time threshold after which an entry is due for a ping sweep.
pub const STALE_AFTER: Duration = Duration::from_secs(900);

/// Neutral RTT used before a peer has been measured (scoring + insert).
pub const UNKNOWN_RTT: Duration = Duration::from_millis(250);

/// EWMA smoothing divisor for RTT samples (α = 1/8).
pub(super) const RTT_DIVISOR: u64 = 8;
