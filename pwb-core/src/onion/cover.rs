//! Cover traffic: decoy cells and randomized dispatch scheduling (TODO §2.3).

use rand::{Rng, RngCore};
use std::time::{Duration, Instant};

use crate::crypto::DhPublicKey;

use super::cell::OnionCell;
use super::constants::{MIN_HOPS, PAYLOAD_MAX};
use super::error::OnionError;
use super::path::{randomize_path_with, RandomizedPath};

/// Cover-traffic timing configuration (TODO §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverConfig {
    /// Mean delay between decoy dispatches. `Duration::ZERO` disables the
    /// random component and always yields [`Self::min_interval`].
    pub mean_interval: Duration,
    /// Lower clamp for a sampled delay (prevents bursts of decoys).
    pub min_interval: Duration,
    /// Upper clamp for a sampled delay (prevents silent gaps).
    pub max_interval: Duration,
}

impl Default for CoverConfig {
    fn default() -> Self {
        Self {
            mean_interval: Duration::from_secs(30),
            min_interval: Duration::from_secs(5),
            max_interval: Duration::from_secs(120),
        }
    }
}

impl CoverConfig {
    /// Sample the delay until the next decoy should be dispatched: an
    /// exponential (memoryless) distribution with the configured mean —
    /// sampled via inverse CDF, `-ln(U) / rate` for `U ~ (0, 1]` — clamped
    /// into `[min_interval, max_interval]` so dispatch never bursts or
    /// stalls. Callers in tests inject a seeded RNG; production callers
    /// pass `OsRng`.
    pub fn sample_delay(&self, rng: &mut impl RngCore) -> Duration {
        let mean = self.mean_interval.as_secs_f64();
        let mut secs: f64 = 0.0;
        if mean > 0.0 {
            let rate = 1.0 / mean;
            if rate.is_finite() && rate > 0.0 {
                let u: f64 = 1.0 - rng.gen::<f64>();
                secs = -u.ln() / rate;
            }
        }
        if !secs.is_finite() {
            secs = 0.0;
        }
        let lo = self.min_interval.as_secs_f64();
        let hi = self.max_interval.as_secs_f64();
        Duration::from_secs_f64(secs.clamp(lo.min(hi), lo.max(hi)))
    }
}

/// Decoy-cell generator with randomized dispatch timing (TODO §2.3).
///
/// Produces cover traffic that is byte-for-byte shaped like a real request:
/// an ordinary [`OnionCell`] (`cell_kind` 0, [`CELL_SIZE`] bytes) over a
/// [`randomize_path`]-randomized path with a uniformly random payload.
/// [`CoverDispatch`] wraps this generator with the due-time scheduling;
/// advanced callers that drive their own loop can use
/// [`Self::sample_next_delay`] + [`Self::decoy_cell`] directly.
///
/// [`CELL_SIZE`]: super::constants::CELL_SIZE
/// [`randomize_path`]: super::path::randomize_path
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverTraffic {
    /// The timing configuration in effect.
    pub config: CoverConfig,
}

impl CoverTraffic {
    /// Create a generator with the given timing configuration.
    pub fn new(config: CoverConfig) -> Self {
        Self { config }
    }

    /// Sample the delay until the next decoy should be dispatched (see
    /// [`CoverConfig::sample_delay`]).
    pub fn sample_next_delay(&self, rng: &mut impl RngCore) -> Duration {
        self.config.sample_delay(rng)
    }

    /// Build one decoy (cover) cell: a uniformly random payload routed over
    /// a path of at least [`MIN_HOPS`] distinct relays drawn from
    /// `candidates`, padded with randomized dummy hops by
    /// [`randomize_path_with`]. Returns the cell plus the path it was built
    /// for — the caller sends the cell to `path[0]` exactly as it would a
    /// real request.
    pub fn decoy_cell(
        &self,
        candidates: &[DhPublicKey],
        rng: &mut impl RngCore,
    ) -> Result<(OnionCell, RandomizedPath), OnionError> {
        // Dedupe the pool: a relay may appear at most once on any path.
        let mut pool: Vec<DhPublicKey> = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if !pool.contains(candidate) {
                pool.push(*candidate);
            }
        }
        if pool.len() < MIN_HOPS {
            return Err(OnionError::PathTooShort { got: pool.len() });
        }

        // Uniform partial shuffle; the first MIN_HOPS entries become the
        // decoy's active route, the rest stay available as dummy hops.
        for i in 0..MIN_HOPS {
            let j = i + rng.gen_range(0..pool.len() - i);
            pool.swap(i, j);
        }
        let route = pool[..MIN_HOPS].to_vec();

        // Random payload of random length: indistinguishable from a real
        // query (the length prefix lives inside the sealed payload region).
        let len = rng.gen_range(0..=PAYLOAD_MAX);
        let mut payload = vec![0u8; len];
        rng.fill_bytes(&mut payload);

        let path = randomize_path_with(&route, &pool, &mut *rng)?;
        let cell = OnionCell::encapsulate(&payload, &path.relays)?;
        Ok((cell, path))
    }
}

/// Cover-traffic dispatch scheduler (TODO §2.3): arms itself with a freshly
/// sampled randomized delay and hands out decoy cells when they come due.
///
/// The transport loop owns the tick — call [`Self::poll`] with the current
/// time on every iteration. Each time the scheduler comes due it re-arms
/// with a **fresh** delay (intervals stay exponential and memoryless, so
/// there is no fixed period for a traffic analyst to lock onto) and returns
/// the next decoy to send. Lateness never accumulates into a burst: one
/// decoy per due poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverDispatch {
    traffic: CoverTraffic,
    next_due: Option<Instant>,
}

impl CoverDispatch {
    /// Create a scheduler with the given timing configuration. Nothing is
    /// due until the first [`Self::poll`] or [`Self::arm`] arms it.
    pub fn new(config: CoverConfig) -> Self {
        Self {
            traffic: CoverTraffic::new(config),
            next_due: None,
        }
    }

    /// The timing configuration in effect.
    pub fn config(&self) -> &CoverConfig {
        &self.traffic.config
    }

    /// When the next decoy is due; `None` until first armed.
    pub fn next_due(&self) -> Option<Instant> {
        self.next_due
    }

    /// (Re)schedule the next decoy for `now + sampled delay`.
    pub fn arm(&mut self, now: Instant, rng: &mut impl RngCore) {
        self.next_due = Some(now + self.traffic.config.sample_delay(rng));
    }

    /// Poll at `now` for a due decoy.
    ///
    /// - First call arms the scheduler and returns `None` — the first decoy
    ///   comes due after the initial sampled delay.
    /// - Before the due time: `None`.
    /// - At/after the due time: re-arms with a fresh delay and returns the
    ///   next decoy cell over `candidates`. If the pool cannot form a path
    ///   (fewer than [`MIN_HOPS`] distinct relays), the attempt is dropped
    ///   and the next one runs at the new due time.
    pub fn poll(
        &mut self,
        now: Instant,
        candidates: &[DhPublicKey],
        rng: &mut impl RngCore,
    ) -> Option<(OnionCell, RandomizedPath)> {
        let Some(due) = self.next_due else {
            self.arm(now, rng);
            return None;
        };
        if now < due {
            return None;
        }
        self.arm(now, rng);
        self.traffic.decoy_cell(candidates, rng).ok()
    }
}
