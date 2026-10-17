use super::*;
use crate::crypto::{DhKeypair, DhPublicKey};
use std::time::{Duration, Instant};

use super::constants::{PAYLOAD_REGION_SIZE, SLOTS_SIZE};

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
        format!("payload of 119 bytes exceeds the per-cell onion capacity of {PAYLOAD_MAX} bytes")
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
