//! Mint policy, service kinds, and refusal reasons.

use std::fmt;
use std::time::Duration;

/// The three verified services that mint credits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceKind {
    /// Serving a `search.plain` query to a requester (challenge round-trip).
    ServeSearchQuery,
    /// Indexing a content-addressed PML bundle.
    IndexBundle,
    /// Relaying onion cells through this node.
    RelayOnionCell,
}

/// Amounts, floors, and windows for [`CreditMinter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintPolicy {
    /// Credits per verified query receipt.
    pub query_credits: u64,
    /// Credits per verified bundle index.
    pub index_credits: u64,
    /// Credits per whole KiB (floor) of sealed relayed cells.
    pub relay_credits_per_kib: u64,
    /// Verified traffic bytes required per credit minted (the anti-ASIC
    /// floor: issuance rate is bounded by network flow, not compute).
    pub traffic_per_credit: u64,
    /// Minimum spacing between mints of the *same* service kind (proof of
    /// time: services must be spread over real time, not burst-computed).
    pub per_service_cooldown: Duration,
    /// Length of one issuance epoch.
    pub epoch: Duration,
    /// Maximum credits mints within one epoch.
    pub epoch_cap: u64,
    /// Lifetime of a requester challenge before it expires unclaimed.
    pub challenge_ttl: Duration,
}

impl Default for MintPolicy {
    fn default() -> Self {
        MintPolicy {
            query_credits: 5,
            index_credits: 10,
            relay_credits_per_kib: 1,
            traffic_per_credit: 1024,
            per_service_cooldown: Duration::from_secs(1),
            epoch: Duration::from_secs(3600),
            epoch_cap: 10_000,
            challenge_ttl: Duration::from_secs(300),
        }
    }
}

/// Why a mint was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintError {
    /// The receipt signature does not verify.
    BadSignature,
    /// The challenge was never issued or was already claimed (replay).
    UnknownChallenge,
    /// The challenge outlived `MintPolicy::challenge_ttl`.
    ChallengeExpired,
    /// The bundle bytes do not hash to the receipt's content id.
    ContentMismatch,
    /// The bundle bytes do not deserialize as a PML bundle.
    BundleUnreadable,
    /// The receipt's term hash does not match the bundle's actual terms.
    TermMismatch,
    /// The receipt's signer is not this minter's owner identity.
    WrongSigner,
    /// The relay receipt does not match the sealed cell chain.
    RelayChainBroken,
    /// `mint_relay` called with no sealed window awaiting claim.
    NoSealedRelayWork,
    /// The per-service cooldown has not elapsed.
    Cooldown { retry_after: Duration },
    /// Verified traffic covers fewer bytes than the mint requires.
    RateLimited {
        needed_bytes: u64,
        available_bytes: u64,
    },
    /// The epoch's issuance cap is exhausted.
    EpochCapReached,
}

impl fmt::Display for MintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MintError::BadSignature => write!(f, "service receipt signature is invalid"),
            MintError::UnknownChallenge => {
                write!(f, "requester challenge is unknown or already claimed")
            }
            MintError::ChallengeExpired => write!(f, "requester challenge expired"),
            MintError::ContentMismatch => {
                write!(f, "bundle bytes do not hash to the receipt content id")
            }
            MintError::BundleUnreadable => write!(f, "bundle bytes do not deserialize"),
            MintError::TermMismatch => {
                write!(f, "receipt term hash does not match the bundle's terms")
            }
            MintError::WrongSigner => {
                write!(f, "receipt was not signed by this node's identity")
            }
            MintError::RelayChainBroken => {
                write!(f, "relay receipt does not match the sealed cell chain")
            }
            MintError::NoSealedRelayWork => {
                write!(f, "no sealed relay window is awaiting mint")
            }
            MintError::Cooldown { retry_after } => {
                write!(f, "service cooldown active, retry in {retry_after:?}")
            }
            MintError::RateLimited {
                needed_bytes,
                available_bytes,
            } => write!(
                f,
                "verified traffic covers {available_bytes} of {needed_bytes} required bytes"
            ),
            MintError::EpochCapReached => write!(f, "epoch credit cap reached"),
        }
    }
}

impl std::error::Error for MintError {}
