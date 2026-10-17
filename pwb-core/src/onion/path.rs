//! Relay-path validation and length randomization (TODO §2.2).

use rand::rngs::OsRng;
use rand::{Rng, RngCore};

use crate::crypto::DhPublicKey;

use super::constants::{MAX_LAYER_SLOTS, MIN_HOPS};
use super::error::OnionError;

/// Check the rules a routed path must satisfy before [`OnionCell::encapsulate`]
/// builds a cell: 3..=6 relays, all distinct.
///
/// Uniqueness is a hard security requirement, not a style rule: two slots
/// addressed to the same relay would encrypt different plaintexts under the
/// same derived key with the same (zero) nonce, destroying the AEAD.
pub fn validate_path(path: &[DhPublicKey]) -> Result<(), OnionError> {
    if path.len() < MIN_HOPS {
        return Err(OnionError::PathTooShort { got: path.len() });
    }
    if path.len() > MAX_LAYER_SLOTS {
        return Err(OnionError::PathTooLong { got: path.len() });
    }
    for i in 0..path.len() {
        for j in (i + 1)..path.len() {
            if path[i] == path[j] {
                return Err(OnionError::DuplicateRelay);
            }
        }
    }
    Ok(())
}

/// A relay path produced by [`randomize_path`] (TODO §2.2): the caller's
/// active route with a uniformly random number of dummy relays interspersed,
/// so the total path length reveals nothing about the destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomizedPath {
    /// Entry-first relay keys, ready for [`OnionCell::encapsulate`].
    pub relays: Vec<DhPublicKey>,
    /// How many of `relays` came from the caller's active route; the
    /// remainder are dummy hops chosen purely to randomize the path length.
    pub active_hops: usize,
}

impl RandomizedPath {
    /// Number of randomized dummy hops in this path.
    pub fn dummy_hops(&self) -> usize {
        self.relays.len() - self.active_hops
    }

    /// The path as a relay slice (entry first, exit last).
    pub fn as_slice(&self) -> &[DhPublicKey] {
        &self.relays
    }
}

impl AsRef<[DhPublicKey]> for RandomizedPath {
    fn as_ref(&self) -> &[DhPublicKey] {
        &self.relays
    }
}

/// Randomize the path length for one request using the system RNG
/// (TODO §2.2). See [`randomize_path_with`] for the algorithm; use that
/// variant directly when the RNG must be injectable (tests, simulations).
pub fn randomize_path(
    route: &[DhPublicKey],
    candidates: &[DhPublicKey],
) -> Result<RandomizedPath, OnionError> {
    randomize_path_with(route, candidates, &mut OsRng)
}

/// Randomize the path length for one request with an injectable RNG
/// (TODO §2.2).
///
/// `route` is the caller's **active** hops — entry first, exit last, at
/// least [`MIN_HOPS`] distinct relays (and at most [`MAX_LAYER_SLOTS`], all
/// enforced by [`validate_path`]). `candidates` is the pool of relays
/// eligible as **dummy** hops: extra nodes chosen purely to pad the path so
/// its length reveals nothing about the destination.
///
/// A uniformly random number of dummies (`0..=` the room left by the cell)
/// is drawn and distributed over the interior gaps of the route, so repeated
/// calls over the same active route yield different total lengths in
/// [`MIN_HOPS`]..=[`MAX_LAYER_SLOTS`]. Properties:
///
/// - The active route's entry stays first and its exit stays last (dummies
///   are inserted strictly between them, never in front of the entry the
///   caller deliberately chose).
/// - Every relay on the result is distinct: dummy candidates that duplicate
///   the route (or each other) are filtered out, because duplicate slots
///   would reuse one layer key with the cell's zero nonce
///   (see [`validate_path`]).
/// - The active route survives as an ordered subsequence of the result.
pub fn randomize_path_with(
    route: &[DhPublicKey],
    candidates: &[DhPublicKey],
    rng: &mut impl RngCore,
) -> Result<RandomizedPath, OnionError> {
    validate_path(route)?;

    // Distinct dummy candidates: never a relay already on the active route.
    let mut dummies: Vec<DhPublicKey> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !route.contains(candidate) && !dummies.contains(candidate) {
            dummies.push(*candidate);
        }
    }

    let max_dummies = (MAX_LAYER_SLOTS - route.len()).min(dummies.len());
    let n_dummies = if max_dummies == 0 {
        0
    } else {
        rng.gen_range(0..=max_dummies)
    };

    // Partial shuffle so the dummy subset is not just "the first k
    // candidates" of the pool.
    for i in 0..n_dummies {
        let j = i + rng.gen_range(0..dummies.len() - i);
        dummies.swap(i, j);
    }

    // Scatter the dummies over the interior gaps (before route[1..]).
    let gaps = route.len() - 1;
    let mut counts = vec![0usize; gaps];
    for _ in 0..n_dummies {
        counts[rng.gen_range(0..gaps)] += 1;
    }

    let mut relays = Vec::with_capacity(route.len() + n_dummies);
    let mut next_dummy = 0;
    relays.push(route[0]);
    for i in 1..route.len() {
        for _ in 0..counts[i - 1] {
            relays.push(dummies[next_dummy]);
            next_dummy += 1;
        }
        relays.push(route[i]);
    }

    debug_assert_eq!(next_dummy, n_dummies);
    debug_assert!(relays.len() <= MAX_LAYER_SLOTS);
    Ok(RandomizedPath {
        active_hops: route.len(),
        relays,
    })
}
