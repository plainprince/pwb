//! Layered Sphinx-Style Onion Cells (`pwb-core::onion`).
//!
//! Every onion-routed request travels as a **fixed-size 512-byte cell**. The
//! uniform size (plus fully-encrypted routing slots and payload) means an
//! observer — including the entry relay — cannot learn the query, the packet
//! type, the path length, or the payload length from the wire image.
//!
//! ## Cell layout (512 bytes)
//!
//! ```text
//! offset  size  field
//! 0       1     cell version (ONION_VERSION)
//! 1       1     cell kind (framing only — never a query/packet type)
//! 2       6     reserved (zero; future: circuit id, cover epoch, flags)
//! 8       32    per-cell ephemeral X25519 public key
//! 40      336   layer slots: 6 x 56-byte slots (40-byte directive + 16-byte Poly1305 tag)
//! 376     136   payload region: 120-byte plaintext + 16-byte Poly1305 tag
//! ```
//!
//! ## Layered encryption
//!
//! The sender picks a path of relays (each identified by a long-term X25519
//! [`DhPublicKey`]) and generates one **ephemeral** X25519 keypair for the
//! cell. Hop `i`'s symmetric keys come from the ECDH shared secret between the
//! cell's ephemeral key and relay `i`'s long-term key:
//!
//! ```text
//! shared_i  = X25519(ephemeral_sk, relay_pk_i)
//! layer_i   = BLAKE3-derive_key("pwb-core onion layer v1", shared_i)
//! payload_i = BLAKE3-derive_key("pwb-core onion payload v1", shared_i)
//! ```
//!
//! Only the sender and relay `i` can compute `shared_i`, so no observer can
//! peel any layer. Because every cell carries a **fresh ephemeral key**, each
//! symmetric key encrypts exactly one slot — ChaCha20-Poly1305 therefore runs
//! with an all-zero nonce and never reuses a key/nonce pair.
//!
//! Slot `i` (a 56-byte AEAD blob) holds hop `i`'s directive. Directives are
//! stacked *inside-out*: the last hop's slot decrypts to `DELIVER`, every
//! earlier slot to `FORWARD <next relay pubkey>`. A relay always peels the
//! **front** slot (slot 0) with its layer key; on `FORWARD` it shifts the
//! remaining slots one position left, re-randomizes the vacated trailing slot,
//! and emits the still-512-byte cell. The payload region rides along untouched
//! until the final hop decrypts it with its payload sub-key.
//!
//! Unused trailing slots (path shorter than 6 hops) are random bytes only the
//! sender could produce — they shift forward as opaque padding, so an
//! observer cannot tell where the last real hop sits. Relays drop cells whose
//! front slot fails authentication (`peel` returns `Ok(None)`), which also
//! covers garbage slots and cells addressed to other relays.
//!
//! ## Roadmap hooks
//!
//! - **Randomized hop counts & dummy nodes** (TODO §2.2): [`randomize_path`]
//!   builds every request's path from a caller-supplied active route
//!   (`MIN_HOPS`=3 .. `MAX_LAYER_SLOTS`=6 hops) plus a uniformly random
//!   number of dummy relays. Directives carry no position, length, or
//!   "final hop" flag, so an intermediate relay cannot tell whether its
//!   [`OnionAction::Forward`] names another relay or the final destination
//!   (locked in by
//!   `test_intermediate_cannot_tell_if_next_hop_is_destination`).
//! - **Cover traffic & metadata protection** (TODO §2.3): [`CoverTraffic`]
//!   builds decoy cells over randomized paths (ordinary `cell_kind` 0 cells,
//!   indistinguishable from real requests), [`CoverDispatch`] schedules them
//!   at exponentially distributed, clamped intervals — one per due poll, no
//!   fixed period — and [`crate::net::CellChannel`] carries every cell
//!   hop-by-hop over UDP re-originated from the forwarding relay's own
//!   socket. The datagram envelope is exactly the cell bytes: no
//!   source-address field exists to carry or forge, constant [`CELL_SIZE`]
//!   datagrams hide sizes, and the remaining unencryptable socket metadata
//!   (prev-relay IP → next-relay IP, randomized timing) carries no
//!   actionable signal about the originator.
//! - **Bulk payloads**: [`PAYLOAD_MAX`] is the per-cell capacity; larger
//!   transfers will fragment into multi-cell streams above this layer.

use crate::crypto::{DhKeypair, DhPublicKey};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::rngs::OsRng;
use rand::{Rng, RngCore};
use std::fmt;
use std::time::{Duration, Instant};
use zeroize::{Zeroize, Zeroizing};

/// Cell format version written into every cell's first byte.
pub const ONION_VERSION: u8 = 1;

/// Fixed onion cell size in bytes. 512 stays far below any IP/UDP MTU (no
/// fragmentation), matches the Sphinx/Tor cell tradition, and makes traffic
/// analysis of query sizes impossible.
pub const CELL_SIZE: usize = 512;

/// Maximum number of layer slots (and therefore onion hops) per cell.
pub const MAX_LAYER_SLOTS: usize = 6;

/// Minimum number of relays on a built path (entry .. exit inclusive).
pub const MIN_HOPS: usize = 3;

/// Maximum payload bytes that fit in one cell (see module docs for the
/// payload-region math: 136 - 16-byte tag - 2-byte length prefix).
pub const PAYLOAD_MAX: usize = 118;

const SLOT_SIZE: usize = 56;
const SLOT_TAG_SIZE: usize = 16;
const SLOT_PLAINTEXT_SIZE: usize = SLOT_SIZE - SLOT_TAG_SIZE;
const HEADER_SIZE: usize = 40;
const SLOTS_SIZE: usize = MAX_LAYER_SLOTS * SLOT_SIZE;
const PAYLOAD_REGION_SIZE: usize = CELL_SIZE - HEADER_SIZE - SLOTS_SIZE;
const PAYLOAD_TAG_SIZE: usize = 16;
const PAYLOAD_LEN_SIZE: usize = 2;
const PAYLOAD_PLAINTEXT_SIZE: usize = PAYLOAD_REGION_SIZE - PAYLOAD_TAG_SIZE;

const KDF_LAYER_CONTEXT: &str = "pwb-core onion layer v1";
const KDF_PAYLOAD_CONTEXT: &str = "pwb-core onion payload v1";

const DIRECTIVE_FORWARD: u8 = 0x01;
const DIRECTIVE_DELIVER: u8 = 0x02;

const ZERO_NONCE: [u8; 12] = [0u8; 12];

/// Errors building or decoding onion cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnionError {
    PayloadTooLarge { got: usize },
    PathTooShort { got: usize },
    PathTooLong { got: usize },
    DuplicateRelay,
    MalformedCell,
    UnsupportedVersion(u8),
}

impl fmt::Display for OnionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OnionError::PayloadTooLarge { got } => write!(
                f,
                "payload of {got} bytes exceeds the per-cell onion capacity of {PAYLOAD_MAX} bytes"
            ),
            OnionError::PathTooShort { got } => write!(
                f,
                "onion path must include at least {MIN_HOPS} relays, got {got}"
            ),
            OnionError::PathTooLong { got } => write!(
                f,
                "onion path exceeds the {MAX_LAYER_SLOTS}-slot cell capacity, got {got}"
            ),
            OnionError::DuplicateRelay => write!(f, "onion path contains a duplicated relay"),
            OnionError::MalformedCell => write!(f, "malformed onion cell"),
            OnionError::UnsupportedVersion(v) => write!(f, "unsupported onion version {v}"),
        }
    }
}

impl std::error::Error for OnionError {}

/// What a relay should do after successfully peeling its layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnionAction {
    /// Hand [`OnionCell`] `forward` to this relay (the next hop on the path).
    Forward(DhPublicKey),
    /// Accept `payload` as the cell's final destination (exit node).
    Deliver,
}

/// Result of a successful [`OnionCell::peel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeeledCell {
    /// The directive decrypted from the front layer slot.
    pub action: OnionAction,
    /// For [`OnionAction::Forward`]: the shifted, re-padded cell to hand to
    /// the next hop (still exactly [`CELL_SIZE`] bytes).
    pub forward: Option<OnionCell>,
    /// For [`OnionAction::Deliver`]: the decrypted payload for this node.
    pub payload: Option<Vec<u8>>,
}

/// A fixed-size onion-routed cell.
///
/// Cells are built with [`OnionCell::encapsulate`], transported opaquely,
/// and peeled relay-by-relay with [`OnionCell::peel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionCell {
    cell_kind: u8,
    ephemeral_pk: DhPublicKey,
    slots: [u8; SLOTS_SIZE],
    payload_region: [u8; PAYLOAD_REGION_SIZE],
}

impl OnionCell {
    /// Build a fixed-size onion cell that wraps `payload` for delivery to the
    /// final relay of `path` (entry first, exit last).
    ///
    /// The path must satisfy [`validate_path`]: 3..=6 unique relays. The
    /// payload must fit in [`PAYLOAD_MAX`] bytes.
    pub fn encapsulate(payload: &[u8], path: &[DhPublicKey]) -> Result<Self, OnionError> {
        validate_path(path)?;
        if payload.len() > PAYLOAD_MAX {
            return Err(OnionError::PayloadTooLarge { got: payload.len() });
        }
        let n = path.len();

        // Fresh ephemeral keypair per cell: every hop's symmetric keys are
        // single-use, so the AEADs below can safely run with a zero nonce.
        let eph = DhKeypair::generate();
        let mut hop_keys = Vec::with_capacity(n);
        for hop in path {
            let shared = Zeroizing::new(eph.diffie_hellman(hop));
            hop_keys.push(derive_layer_keys(&shared));
        }

        // Slot stack: random fill first (the unused trailing slots become
        // opaque padding), then the real directives written inside-out.
        let mut slots = [0u8; SLOTS_SIZE];
        OsRng.fill_bytes(&mut slots);
        for i in (0..n).rev() {
            let mut directive = [0u8; SLOT_PLAINTEXT_SIZE];
            OsRng.fill_bytes(&mut directive);
            directive[0] = if i + 1 == n {
                DIRECTIVE_DELIVER
            } else {
                DIRECTIVE_FORWARD
            };
            if i + 1 != n {
                directive[1..33].copy_from_slice(&path[i + 1].0);
            }
            let sealed = seal(&hop_keys[i].0, &directive)
                .expect("slot plaintext is fixed-size; ChaCha20-Poly1305 never fails to seal");
            slots[i * SLOT_SIZE..(i + 1) * SLOT_SIZE].copy_from_slice(&sealed);
            directive.zeroize();
        }

        // Payload region: fixed plaintext size with a length prefix and random
        // padding so the ciphertext length never leaks the payload size.
        let mut payload_plain = [0u8; PAYLOAD_PLAINTEXT_SIZE];
        OsRng.fill_bytes(&mut payload_plain);
        payload_plain[..PAYLOAD_LEN_SIZE].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        payload_plain[PAYLOAD_LEN_SIZE..PAYLOAD_LEN_SIZE + payload.len()].copy_from_slice(payload);
        let payload_region: [u8; PAYLOAD_REGION_SIZE] = seal(&hop_keys[n - 1].1, &payload_plain)
            .expect("payload plaintext is fixed-size; ChaCha20-Poly1305 never fails to seal")
            .try_into()
            .expect("sealed payload matches the payload region size");
        payload_plain.zeroize();

        Ok(Self {
            cell_kind: 0,
            ephemeral_pk: eph.public,
            slots,
            payload_region,
        })
    }

    /// Attempt to peel this cell as `relay` (its long-term identity keypair).
    ///
    /// Returns:
    /// - `Ok(Some(peeled))` — the front slot authenticated under the relay's
    ///   layer key; `peeled.action` says whether to forward or deliver.
    /// - `Ok(None)` — the front slot did not authenticate (the cell is not
    ///   addressed to this relay at this position, or it hit a garbage slot);
    ///   the relay should silently drop the cell.
    /// - `Err` — the cell payload failed authentication for the exit node or
    ///   the directive was corrupt (a sender or memory bug, not wire traffic).
    pub fn peel(&self, relay: &DhKeypair) -> Result<Option<PeeledCell>, OnionError> {
        let shared = Zeroizing::new(relay.diffie_hellman(&self.ephemeral_pk));
        let (layer_key, payload_key) = derive_layer_keys(&shared);

        let front = &self.slots[..SLOT_SIZE];
        let Some(directive) = open(&layer_key, front) else {
            return Ok(None);
        };
        let directive = Zeroizing::new(directive);

        match directive[0] {
            DIRECTIVE_FORWARD => {
                let mut next = [0u8; 32];
                next.copy_from_slice(&directive[1..33]);
                let forward = self.shifted_forward_cell();
                Ok(Some(PeeledCell {
                    action: OnionAction::Forward(DhPublicKey(next)),
                    forward: Some(forward),
                    payload: None,
                }))
            }
            DIRECTIVE_DELIVER => {
                let payload_plain =
                    open(&payload_key, &self.payload_region).ok_or(OnionError::MalformedCell)?;
                let len = u16::from_le_bytes([payload_plain[0], payload_plain[1]]) as usize;
                let end = PAYLOAD_LEN_SIZE + len;
                if end > payload_plain.len() {
                    return Err(OnionError::MalformedCell);
                }
                Ok(Some(PeeledCell {
                    action: OnionAction::Deliver,
                    forward: None,
                    payload: Some(payload_plain[PAYLOAD_LEN_SIZE..end].to_vec()),
                }))
            }
            _ => Err(OnionError::MalformedCell),
        }
    }

    /// The cell's wire bytes (exactly [`CELL_SIZE`] bytes).
    pub fn to_bytes(&self) -> [u8; CELL_SIZE] {
        let mut out = [0u8; CELL_SIZE];
        out[0] = ONION_VERSION;
        out[1] = self.cell_kind;
        // bytes 2..8 stay zero (reserved)
        out[8..40].copy_from_slice(&self.ephemeral_pk.0);
        out[40..40 + SLOTS_SIZE].copy_from_slice(&self.slots);
        out[40 + SLOTS_SIZE..].copy_from_slice(&self.payload_region);
        out
    }

    /// Decode a cell from its wire bytes. Rejects wrong sizes and versions.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, OnionError> {
        if bytes.len() != CELL_SIZE {
            return Err(OnionError::MalformedCell);
        }
        if bytes[0] != ONION_VERSION {
            return Err(OnionError::UnsupportedVersion(bytes[0]));
        }
        let mut ephemeral_pk = [0u8; 32];
        ephemeral_pk.copy_from_slice(&bytes[8..40]);
        let mut slots = [0u8; SLOTS_SIZE];
        slots.copy_from_slice(&bytes[40..40 + SLOTS_SIZE]);
        let mut payload_region = [0u8; PAYLOAD_REGION_SIZE];
        payload_region.copy_from_slice(&bytes[40 + SLOTS_SIZE..]);
        Ok(Self {
            cell_kind: bytes[1],
            ephemeral_pk: DhPublicKey(ephemeral_pk),
            slots,
            payload_region,
        })
    }

    /// The cell's routing slots on the wire (opaque 336 bytes).
    pub fn slots(&self) -> &[u8; SLOTS_SIZE] {
        &self.slots
    }

    /// The cell's per-cell ephemeral X25519 public key.
    pub fn ephemeral_pk(&self) -> DhPublicKey {
        self.ephemeral_pk
    }

    /// Shift the slot stack one position left (the front slot was peeled) and
    /// re-randomize the vacated trailing slot so the next hop — and any
    /// observer — sees the same opaque 512-byte shape. The header and payload
    /// region are untouched; the payload stays sealed for the exit node.
    fn shifted_forward_cell(&self) -> Self {
        let mut slots = [0u8; SLOTS_SIZE];
        slots[..SLOTS_SIZE - SLOT_SIZE].copy_from_slice(&self.slots[SLOT_SIZE..]);
        OsRng.fill_bytes(&mut slots[SLOTS_SIZE - SLOT_SIZE..]);
        Self {
            cell_kind: self.cell_kind,
            ephemeral_pk: self.ephemeral_pk,
            slots,
            payload_region: self.payload_region,
        }
    }
}

impl fmt::Display for OnionCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "OnionCell {{ version: {}, kind: {}, eph: {}, slots: {}B, payload: {}B }}",
            ONION_VERSION,
            self.cell_kind,
            self.ephemeral_pk.to_hex(),
            SLOTS_SIZE,
            PAYLOAD_REGION_SIZE
        )
    }
}

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

/// Derive the per-hop symmetric sub-keys from an ECDH shared secret. Separate
/// BLAKE3 contexts keep the layer key and payload key independent, so the exit
/// node never reuses one key/nonce pair for both its slot and the payload.
fn derive_layer_keys(shared: &[u8; 32]) -> (Zeroizing<[u8; 32]>, Zeroizing<[u8; 32]>) {
    (
        Zeroizing::new(blake3::derive_key(KDF_LAYER_CONTEXT, shared)),
        Zeroizing::new(blake3::derive_key(KDF_PAYLOAD_CONTEXT, shared)),
    )
}

fn seal(key: &[u8; 32], plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .encrypt(Nonce::from_slice(&ZERO_NONCE), plaintext)
        .ok()
}

fn open(key: &[u8; 32], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(&ZERO_NONCE), ciphertext)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_of(n: usize) -> Vec<DhKeypair> {
        (0..n).map(|_| DhKeypair::generate()).collect()
    }

    fn pubkeys(keys: &[DhKeypair]) -> Vec<DhPublicKey> {
        keys.iter().map(|k| k.public).collect()
    }

    #[test]
    fn test_onion_roundtrip_through_full_path() {
        let relays = path_of(3);
        let path = pubkeys(&relays);
        let payload = b"q=untrackable";

        let cell = OnionCell::encapsulate(payload, &path).expect("build cell");
        assert_eq!(cell.to_bytes().len(), CELL_SIZE);

        // Entry relay peels and forwards.
        let peeled = cell
            .peel(&relays[0])
            .expect("entry peel succeeds")
            .expect("slot authenticates");
        let OnionAction::Forward(next) = peeled.action else {
            panic!("entry must forward, got {:?}", peeled.action);
        };
        assert_eq!(next, path[1], "forward directive names the next relay");
        let cell = peeled.forward.expect("forward carries the next cell");
        assert_eq!(
            cell.to_bytes().len(),
            CELL_SIZE,
            "forwarded cell stays fixed-size"
        );

        // Middle relay peels and forwards.
        let peeled = cell
            .peel(&relays[1])
            .expect("middle peel succeeds")
            .expect("slot authenticates");
        let OnionAction::Forward(next) = peeled.action else {
            panic!("middle must forward, got {:?}", peeled.action);
        };
        assert_eq!(next, path[2]);
        let cell = peeled.forward.expect("forward carries the next cell");
        assert_eq!(cell.to_bytes().len(), CELL_SIZE);

        // Exit relay peels and receives the payload.
        let peeled = cell
            .peel(&relays[2])
            .expect("exit peel succeeds")
            .expect("slot authenticates");
        assert_eq!(peeled.action, OnionAction::Deliver);
        assert!(peeled.forward.is_none());
        assert_eq!(peeled.payload.as_deref(), Some(payload.as_slice()));
    }

    #[test]
    fn test_onion_roundtrip_six_hops_and_empty_payload() {
        let relays = path_of(MAX_LAYER_SLOTS);
        let path = pubkeys(&relays);
        let mut cell = OnionCell::encapsulate(b"", &path).expect("build cell");

        for (i, relay) in relays.iter().enumerate() {
            let peeled = cell
                .peel(relay)
                .expect("peel succeeds")
                .expect("slot authenticates");
            if i + 1 == relays.len() {
                assert_eq!(peeled.action, OnionAction::Deliver);
                assert_eq!(peeled.payload, Some(Vec::new()));
            } else {
                assert_eq!(
                    peeled.action,
                    OnionAction::Forward(path[i + 1]),
                    "hop {} forwards correctly",
                    i
                );
                cell = peeled.forward.expect("forward cell");
            }
        }
    }

    #[test]
    fn test_cell_is_fixed_size_at_every_hop() {
        let relays = path_of(4);
        let path = pubkeys(&relays);
        let mut cell = OnionCell::encapsulate(&[0xAB; PAYLOAD_MAX], &path).expect("build cell");

        for relay in &relays[..relays.len() - 1] {
            let bytes = cell.to_bytes();
            assert_eq!(bytes.len(), CELL_SIZE);
            let decoded = OnionCell::from_bytes(&bytes).expect("wire roundtrip");
            let peeled = decoded.peel(relay).expect("peel").expect("authenticates");
            cell = peeled.forward.expect("forward");
        }
        assert_eq!(cell.to_bytes().len(), CELL_SIZE);
    }

    #[test]
    fn test_header_survives_peeling_unchanged() {
        // The ephemeral key must stay intact across hops so each relay can
        // derive its own layer keys from the same per-cell ECDH material.
        let relays = path_of(3);
        let path = pubkeys(&relays);
        let cell = OnionCell::encapsulate(b"ping", &path).expect("build cell");
        let eph = cell.ephemeral_pk();

        let mut cell = cell;
        for relay in &relays[..2] {
            let peeled = cell.peel(relay).expect("peel").expect("authenticates");
            cell = peeled.forward.expect("forward");
            assert_eq!(
                cell.ephemeral_pk(),
                eph,
                "ephemeral key preserved across peels"
            );
        }
    }

    #[test]
    fn test_wrong_relay_cannot_peel() {
        let relays = path_of(3);
        let outsider = DhKeypair::generate();
        let path = pubkeys(&relays);
        let cell = OnionCell::encapsulate(b"secret query", &path).expect("build cell");

        // A relay that is not on the path fails the front-slot MAC.
        assert_eq!(cell.peel(&outsider).expect("no protocol error"), None);

        // A relay on the path but at the wrong position also fails: slot 0 is
        // encrypted for the entry relay only.
        assert_eq!(cell.peel(&relays[2]).expect("no protocol error"), None);
    }

    #[test]
    fn test_tampered_cells_are_rejected() {
        let relays = path_of(3);
        let path = pubkeys(&relays);
        let cell = OnionCell::encapsulate(b"q=untrackable", &path).expect("build cell");

        // Tampered slot → entry cannot peel (silent drop).
        let mut bytes = cell.to_bytes();
        bytes[40] ^= 0x01;
        let tampered = OnionCell::from_bytes(&bytes).expect("version unchanged");
        assert_eq!(tampered.peel(&relays[0]).expect("no protocol error"), None);

        // Tampered payload region → entry still forwards (payload is not its
        // layer), but the exit's payload authentication fails loudly.
        let mut bytes = cell.to_bytes();
        bytes[CELL_SIZE - 1] ^= 0x01;
        let tampered = OnionCell::from_bytes(&bytes).expect("version unchanged");
        let peeled = tampered
            .peel(&relays[0])
            .expect("entry peels")
            .expect("authenticates");
        let cell = peeled.forward.expect("forward");
        let peeled = cell
            .peel(&relays[1])
            .expect("middle peels")
            .expect("authenticates");
        let cell = peeled.forward.expect("forward");
        assert_eq!(cell.peel(&relays[2]), Err(OnionError::MalformedCell));

        // Wrong version is rejected at decode time.
        let mut bytes = cell.to_bytes();
        bytes[0] = ONION_VERSION + 1;
        assert_eq!(
            OnionCell::from_bytes(&bytes),
            Err(OnionError::UnsupportedVersion(ONION_VERSION + 1))
        );

        // Wrong size is rejected at decode time.
        assert_eq!(
            OnionCell::from_bytes(&bytes[..CELL_SIZE - 1]),
            Err(OnionError::MalformedCell)
        );
    }

    #[test]
    fn test_path_and_payload_validation() {
        let relays = path_of(MAX_LAYER_SLOTS);
        let path = pubkeys(&relays);

        assert_eq!(
            OnionCell::encapsulate(b"x", &path[..MIN_HOPS - 1]),
            Err(OnionError::PathTooShort { got: MIN_HOPS - 1 })
        );
        assert_eq!(
            OnionCell::encapsulate(
                b"x",
                &relays
                    .iter()
                    .map(|k| k.public)
                    .chain(std::iter::once(DhKeypair::generate().public))
                    .collect::<Vec<_>>()
            ),
            Err(OnionError::PathTooLong {
                got: MAX_LAYER_SLOTS + 1
            })
        );
        assert_eq!(
            OnionCell::encapsulate(b"x", &[path[0], path[1], path[0]]),
            Err(OnionError::DuplicateRelay)
        );
        assert_eq!(
            OnionCell::encapsulate(&[0u8; PAYLOAD_MAX + 1], &path[..MIN_HOPS]),
            Err(OnionError::PayloadTooLarge {
                got: PAYLOAD_MAX + 1
            })
        );

        // Boundary sizes build fine.
        assert!(OnionCell::encapsulate(&[0u8; PAYLOAD_MAX], &path[..MIN_HOPS]).is_ok());
        assert!(OnionCell::encapsulate(b"x", &path).is_ok());
    }

    #[test]
    fn test_peel_does_not_leak_payload_to_intermediates() {
        let relays = path_of(3);
        let path = pubkeys(&relays);
        let payload = b"highly private query";
        let mut cell = OnionCell::encapsulate(payload, &path).expect("build cell");

        for relay in &relays[..2] {
            let peeled = cell.peel(relay).expect("peel").expect("authenticates");
            assert!(
                peeled.payload.is_none(),
                "intermediates never see the payload"
            );
            cell = peeled.forward.expect("forward");
        }
    }

    #[test]
    fn test_wire_image_is_uniform_across_paths() {
        // Two cells for different path lengths must be byte-identical in
        // structure: same fixed size, same reserved bytes, and slot regions
        // that no observer can align to a real/garbage boundary.
        let relays = path_of(MAX_LAYER_SLOTS);
        let short = OnionCell::encapsulate(b"q=a", &pubkeys(&relays)[..MIN_HOPS]).unwrap();
        let long = OnionCell::encapsulate(b"q=a", &pubkeys(&relays)).unwrap();
        assert_eq!(short.to_bytes().len(), long.to_bytes().len());
        assert_eq!(
            short.to_bytes()[..8],
            long.to_bytes()[..8],
            "matching framing bytes"
        );
    }

    #[test]
    fn test_randomized_path_length_varies_per_request() {
        use rand::{rngs::StdRng, SeedableRng};
        use std::collections::BTreeSet;

        let route = pubkeys(&path_of(MIN_HOPS));
        let candidates = pubkeys(&path_of(6));
        let mut rng = StdRng::seed_from_u64(0xC0FFEE);
        let mut seen_lengths = BTreeSet::new();

        for _ in 0..256 {
            let path = randomize_path_with(&route, &candidates, &mut rng)
                .expect("valid route and pool randomize");

            assert!(
                (MIN_HOPS..=MAX_LAYER_SLOTS).contains(&path.relays.len()),
                "length {} outside bounds",
                path.relays.len()
            );
            assert_eq!(path.active_hops, route.len());
            assert_eq!(path.relays.len(), path.active_hops + path.dummy_hops());

            // Entry and exit of the active route are preserved.
            assert_eq!(path.relays.first(), route.first());
            assert_eq!(path.relays.last(), route.last());

            // Filtering the dummies out leaves exactly the active route, in
            // order (dummies are disjoint from the route by construction).
            let filtered: Vec<DhPublicKey> = path
                .relays
                .iter()
                .copied()
                .filter(|relay| route.contains(relay))
                .collect();
            assert_eq!(filtered, route, "active route survives as subsequence");

            // Every dummy comes from the candidate pool.
            for relay in path.relays.iter().filter(|r| !route.contains(r)) {
                assert!(candidates.contains(relay), "dummy must come from pool");
            }

            // Unique-relay rule holds on the built path.
            validate_path(path.as_slice()).expect("randomized path validates");

            seen_lengths.insert(path.relays.len());
        }

        assert!(
            seen_lengths.len() > 1,
            "path length must be randomized across requests, saw only {seen_lengths:?}"
        );
    }

    #[test]
    fn test_randomize_path_validation_and_bounds() {
        use rand::{rngs::StdRng, SeedableRng};

        let mut rng = StdRng::seed_from_u64(7);
        let keys = path_of(MAX_LAYER_SLOTS);
        let route = pubkeys(&keys);

        // The active route itself must be valid.
        assert_eq!(
            randomize_path_with(&route[..MIN_HOPS - 1], &route, &mut rng),
            Err(OnionError::PathTooShort { got: MIN_HOPS - 1 })
        );
        let too_long: Vec<DhPublicKey> = route
            .iter()
            .copied()
            .chain(std::iter::once(DhKeypair::generate().public))
            .collect();
        assert_eq!(
            randomize_path_with(&too_long, &[], &mut rng),
            Err(OnionError::PathTooLong {
                got: MAX_LAYER_SLOTS + 1
            })
        );
        assert_eq!(
            randomize_path_with(&[route[0], route[1], route[0]], &route, &mut rng),
            Err(OnionError::DuplicateRelay)
        );

        // A route already at capacity never gains dummy hops, even with a
        // large pool.
        let full = randomize_path_with(&route, &pubkeys(&path_of(3)), &mut rng).unwrap();
        assert_eq!(full.relays.len(), MAX_LAYER_SLOTS);
        assert_eq!(full.dummy_hops(), 0);

        // Empty candidate pool → the path is just the active route.
        let bare = randomize_path_with(&route[..MIN_HOPS], &[], &mut rng).unwrap();
        assert_eq!(bare.relays, route[..MIN_HOPS].to_vec());
        assert_eq!(bare.dummy_hops(), 0);

        // Candidates that overlap the route (or duplicate each other) are
        // filtered, never producing duplicate relays.
        let mut dups = route[..MIN_HOPS].to_vec();
        dups.extend_from_slice(&route[..2]);
        dups.push(dups[0]);
        let overlapped = randomize_path_with(&route[..MIN_HOPS], &dups, &mut rng).unwrap();
        validate_path(overlapped.as_slice()).expect("no duplicate relays");
        assert_eq!(overlapped.relays, route[..MIN_HOPS].to_vec());

        // The OsRng convenience wrapper behaves like the injectable one.
        let system = randomize_path(&route[..MIN_HOPS], &pubkeys(&path_of(4))).unwrap();
        assert!((MIN_HOPS..=MAX_LAYER_SLOTS).contains(&system.relays.len()));
        validate_path(system.as_slice()).expect("valid system-randomized path");
    }

    /// Peel a cell through `keys[0]` (entry) and `keys[1]` (middle), and
    /// return everything the middle relay can observe plus the cell it would
    /// forward. `PeeledCell`'s public fields are the *entire* observable —
    /// there is no position, path-length, or "next hop is the destination"
    /// field anywhere in it, and the forwarded cell's header is fixed
    /// framing (its slot ciphertext is uniformly random in both worlds, so
    /// only the deterministic framing is included in the comparison).
    fn middle_relay_view(cell: &OnionCell, keys: &[DhKeypair]) -> (String, OnionCell) {
        let entry = cell
            .peel(&keys[0])
            .expect("entry peel")
            .expect("authenticates");
        let OnionAction::Forward(mid) = entry.action else {
            panic!("entry must forward, got {:?}", entry.action);
        };
        assert_eq!(mid, keys[1].public);

        let middle = entry
            .forward
            .expect("entry forward cell")
            .peel(&keys[1])
            .expect("middle peel")
            .expect("authenticates");
        let PeeledCell {
            action,
            forward,
            payload,
        } = middle;
        let OnionAction::Forward(next) = action else {
            panic!("middle must forward, got {action:?}");
        };
        assert!(payload.is_none(), "middle never sees a payload");
        let forward = forward.expect("middle forward cell");

        let bytes = forward.to_bytes();
        let view = format!(
            "{next:?}|payload={}|wire={}B|framing={:02x?}",
            payload.is_some(),
            bytes.len(),
            &bytes[..8]
        );
        (view, forward)
    }

    #[test]
    fn test_intermediate_cannot_tell_if_next_hop_is_destination() {
        // Two worlds that differ only DOWNSTREAM of the middle relay's next
        // hop:
        //   world A: [entry, middle, exit]      — next hop IS the destination.
        //   world B: [entry, middle, relay, exit] — next hop relays onward.
        // If the middle relay's observable differed in any way, it could
        // determine whether its next hop was the final destination.
        let keys = path_of(4);
        let world_a = pubkeys(&keys[..3]);
        let world_b = pubkeys(&keys);

        let cell_a = OnionCell::encapsulate(b"query", &world_a).expect("world A");
        let cell_b = OnionCell::encapsulate(b"query", &world_b).expect("world B");

        let (view_a, forward_a) = middle_relay_view(&cell_a, &keys);
        let (view_b, forward_b) = middle_relay_view(&cell_b, &keys);
        assert_eq!(
            view_a, view_b,
            "the middle relay's entire observable must be identical whether \
             or not its next hop is the final destination"
        );

        // The middle relay learns nothing more from the cell it forwards:
        // it no longer authenticates under its own key, and its forward
        // framing (version/kind/reserved/size) is unchanged.
        assert_eq!(
            forward_a.peel(&keys[1]).expect("no protocol error"),
            None,
            "middle cannot re-peek the forwarded cell"
        );
        assert_eq!(forward_b.peel(&keys[1]).expect("no protocol error"), None);

        // The worlds really do diverge downstream — information the middle
        // relay cannot obtain: in world A the next hop delivers...
        let exit_a = forward_a
            .peel(&keys[2])
            .expect("world A exit peel")
            .expect("authenticates");
        assert_eq!(exit_a.action, OnionAction::Deliver);
        assert_eq!(exit_a.payload.as_deref(), Some(b"query".as_slice()));

        // ...while in world B the same relay just forwards onward.
        let hop3 = forward_b
            .peel(&keys[2])
            .expect("world B relay peel")
            .expect("authenticates");
        let OnionAction::Forward(next) = hop3.action else {
            panic!("world B hop 3 must forward, got {:?}", hop3.action);
        };
        assert_eq!(next, keys[3].public);
    }

    #[test]
    fn test_cover_delay_is_randomized_and_clamped() {
        use rand::{rngs::StdRng, SeedableRng};
        use std::collections::BTreeSet;

        let traffic = CoverTraffic::new(CoverConfig::default());
        let mut rng = StdRng::seed_from_u64(99);

        let samples = 512;
        let mut sum = 0.0;
        let mut distinct = BTreeSet::new();
        for _ in 0..samples {
            let delay = traffic.sample_next_delay(&mut rng);
            assert!(
                (traffic.config.min_interval..=traffic.config.max_interval).contains(&delay),
                "delay {delay:?} outside clamps"
            );
            sum += delay.as_secs_f64();
            distinct.insert(delay);
        }
        assert!(
            distinct.len() > 1,
            "dispatch timing must be randomized, not a fixed period"
        );

        // Exponential mean 30s clamped to [5s, 120s] sits near ~32s; assert
        // a loose band so the test documents the distribution without being
        // statistically brittle.
        let mean = traffic.config.mean_interval.as_secs_f64();
        let avg = sum / samples as f64;
        assert!(
            ((mean / 3.0)..(mean * 3.0)).contains(&avg),
            "sampled mean {avg}s not near configured mean {mean}s"
        );
    }

    #[test]
    fn test_cover_delay_respects_degenerate_config() {
        use rand::{rngs::StdRng, SeedableRng};

        let mut rng = StdRng::seed_from_u64(1);

        // mean = 0 → the random component is disabled; always min.
        let steady = CoverTraffic::new(CoverConfig {
            mean_interval: Duration::ZERO,
            min_interval: Duration::from_secs(4),
            max_interval: Duration::from_secs(9),
        });
        for _ in 0..8 {
            assert_eq!(
                steady.sample_next_delay(&mut rng),
                Duration::from_secs(4),
                "zero mean always yields the minimum delay"
            );
        }

        // Misconfigured min > max must not panic; it clamps sanely.
        let inverted = CoverTraffic::new(CoverConfig {
            mean_interval: Duration::from_secs(10),
            min_interval: Duration::from_secs(8),
            max_interval: Duration::from_secs(3),
        });
        for _ in 0..8 {
            let delay = inverted.sample_next_delay(&mut rng);
            assert!((Duration::from_secs(3)..=Duration::from_secs(8)).contains(&delay));
        }
    }

    #[test]
    fn test_decoy_cell_walks_randomized_path() {
        use rand::{rngs::StdRng, SeedableRng};

        let traffic = CoverTraffic::new(CoverConfig::default());
        let keys = path_of(6);
        let candidates = pubkeys(&keys);
        let mut rng = StdRng::seed_from_u64(3);

        let (cell, path) = traffic
            .decoy_cell(&candidates, &mut rng)
            .expect("six candidates are plenty");

        // Same wire shape as a real request.
        assert_eq!(cell.to_bytes().len(), CELL_SIZE);
        assert!((MIN_HOPS..=MAX_LAYER_SLOTS).contains(&path.relays.len()));
        validate_path(path.as_slice()).expect("decoy path validates");

        // Two decoys are independent cells (fresh ephemeral keys).
        let (cell2, _) = traffic
            .decoy_cell(&candidates, &mut rng)
            .expect("second decoy builds");
        assert_ne!(cell.ephemeral_pk(), cell2.ephemeral_pk());

        // The decoy walks its whole path: every relay peels in order and
        // the exit delivers the random payload.
        let key_for = |pk: &DhPublicKey| {
            keys.iter()
                .find(|k| k.public == *pk)
                .expect("path relay comes from candidates")
        };
        let mut current = cell;
        for (i, relay_pk) in path.relays.iter().enumerate() {
            let peeled = current
                .peel(key_for(relay_pk))
                .expect("peel succeeds")
                .expect("slot authenticates");
            if i + 1 == path.relays.len() {
                assert_eq!(peeled.action, OnionAction::Deliver);
                let payload = peeled.payload.expect("exit receives payload");
                assert!(payload.len() <= PAYLOAD_MAX);
            } else {
                let OnionAction::Forward(next) = peeled.action else {
                    panic!("hop {i} must forward");
                };
                assert_eq!(next, path.relays[i + 1]);
                current = peeled.forward.expect("forward cell");
            }
        }

        // Fewer than MIN_HOPS distinct candidates cannot form a decoy path.
        assert_eq!(
            traffic.decoy_cell(&candidates[..MIN_HOPS - 1], &mut rng),
            Err(OnionError::PathTooShort { got: MIN_HOPS - 1 })
        );

        // Duplicate entries in the pool are deduplicated, not rejected.
        let mut dups = candidates.clone();
        dups.extend_from_slice(&candidates[..2]);
        assert!(traffic.decoy_cell(&dups, &mut rng).is_ok());
    }

    #[test]
    fn test_cover_dispatch_schedules_decoys_at_random_intervals() {
        use rand::{rngs::StdRng, SeedableRng};
        use std::collections::BTreeSet;

        let candidates = pubkeys(&path_of(6));
        let mut rng = StdRng::seed_from_u64(11);
        let mut dispatch = CoverDispatch::new(CoverConfig::default());

        let t0 = Instant::now();

        // First poll arms the scheduler; nothing is due yet.
        assert!(dispatch.poll(t0, &candidates, &mut rng).is_none());
        let due = dispatch.next_due().expect("armed by first poll");
        assert!(due > t0);

        // Before the due time → nothing.
        assert!(dispatch
            .poll(due - Duration::from_millis(1), &candidates, &mut rng)
            .is_none());

        // At the due time → a decoy, and the scheduler re-arms with a fresh
        // delay strictly after the one that just fired.
        let (cell, path) = dispatch
            .poll(due, &candidates, &mut rng)
            .expect("due poll emits a decoy");
        assert_eq!(cell.to_bytes().len(), CELL_SIZE);
        validate_path(path.as_slice()).expect("decoy path validates");
        let due2 = dispatch.next_due().expect("re-armed after firing");
        assert!(due2 > due);

        // Intervals stay randomized across re-arms: step time to each due in
        // turn and collect the sampled spacings.
        let mut spacings = BTreeSet::new();
        for _ in 0..16 {
            let now = dispatch.next_due().expect("armed");
            assert!(dispatch.poll(now, &candidates, &mut rng).is_some());
            let next = dispatch.next_due().expect("re-armed");
            spacings.insert(next - now);
        }
        assert!(
            spacings.len() > 1,
            "dispatch intervals must vary, saw only {spacings:?}"
        );
    }

    #[test]
    fn test_cover_dispatch_drops_bad_pools_and_accepts_manual_arm() {
        use rand::{rngs::StdRng, SeedableRng};

        let mut rng = StdRng::seed_from_u64(5);
        let mut dispatch = CoverDispatch::new(CoverConfig::default());
        let t0 = Instant::now();

        // Pool too small for a path: the attempt is dropped, but the
        // scheduler still re-arms so it retries at the next due time.
        assert!(dispatch.poll(t0, &[], &mut rng).is_none());
        let due = dispatch.next_due().expect("armed");
        let too_small = [DhKeypair::generate().public];
        assert!(
            dispatch.poll(due, &too_small, &mut rng).is_none(),
            "cannot build a decoy from fewer than MIN_HOPS relays"
        );
        let rearmed = dispatch.next_due().expect("failed attempt re-arms");
        assert!(rearmed > due);

        // arm() reschedules explicitly (e.g., after a config change).
        let base = t0 + Duration::from_secs(3600);
        dispatch.arm(base, &mut rng);
        let manual = dispatch.next_due().expect("manually armed");
        assert!(
            manual >= base + CoverConfig::default().min_interval,
            "manual arm still respects the sampled delay bounds"
        );
    }

    #[test]
    fn test_display_impls_and_accessors() {
        use rand::{rngs::StdRng, SeedableRng};

        // OnionError Display covers every variant with the real limits.
        assert_eq!(
            OnionError::PayloadTooLarge { got: 119 }.to_string(),
            format!(
                "payload of 119 bytes exceeds the per-cell onion capacity of {PAYLOAD_MAX} bytes"
            )
        );
        assert_eq!(
            OnionError::PathTooShort { got: 2 }.to_string(),
            format!("onion path must include at least {MIN_HOPS} relays, got 2")
        );
        assert_eq!(
            OnionError::PathTooLong { got: 7 }.to_string(),
            format!("onion path exceeds the {MAX_LAYER_SLOTS}-slot cell capacity, got 7")
        );
        assert_eq!(
            OnionError::DuplicateRelay.to_string(),
            "onion path contains a duplicated relay"
        );
        assert_eq!(
            OnionError::MalformedCell.to_string(),
            "malformed onion cell"
        );
        assert_eq!(
            OnionError::UnsupportedVersion(9).to_string(),
            "unsupported onion version 9"
        );

        // Cell Display reports the wire geometry; slots() exposes it too.
        let cell = OnionCell::encapsulate(b"x", &pubkeys(&path_of(MIN_HOPS))).unwrap();
        let shown = cell.to_string();
        assert!(shown.contains(&format!("version: {}", ONION_VERSION)));
        assert!(shown.contains("kind: 0"));
        assert!(shown.contains(&format!("slots: {SLOTS_SIZE}B")));
        assert!(shown.contains(&format!("payload: {PAYLOAD_REGION_SIZE}B")));
        assert_eq!(cell.slots().len(), SLOTS_SIZE);

        // RandomizedPath::AsRef agrees with as_slice (both feed encapsulate).
        let mut rng = StdRng::seed_from_u64(77);
        let route = pubkeys(&path_of(MIN_HOPS));
        let pool = pubkeys(&path_of(4));
        let path = randomize_path_with(&route, &pool, &mut rng).unwrap();
        assert_eq!(path.as_ref(), path.as_slice());
        assert!(OnionCell::encapsulate(b"x", path.as_ref()).is_ok());

        // CoverDispatch::config exposes the timing configuration in effect.
        let dispatch = CoverDispatch::new(CoverConfig::default());
        assert_eq!(*dispatch.config(), CoverConfig::default());
        assert!(dispatch.next_due().is_none());
    }
}
