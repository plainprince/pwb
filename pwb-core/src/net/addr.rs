//! Address-family helpers for dual-stack (IPv4 + IPv6) sockets.
//!
//! A PlainWeb node binds exactly one UDP socket, and that socket's family
//! decides which peers it can reach: an IPv6 socket carries IPv4 traffic only
//! when the peer address is written v4-mapped (`::ffff:a.b.c.d`), and an IPv4
//! socket cannot reach IPv6 peers at all. These helpers keep addresses
//! canonical on the way in and correctly formed on the way out, so callers
//! work with ordinary `SocketAddr` values and never handle mapping
//! themselves.

use std::net::{IpAddr, SocketAddr};

/// Rewrite `dest` for sending from a socket bound to `local`.
///
/// An IPv6-bound socket — including the dual-stack `[::]` bind — must address
/// an IPv4 peer v4-mapped or the kernel rejects the datagram (`EINVAL`). Every
/// other combination passes through unchanged: same-family destinations are
/// already correct, and the remaining mismatches (IPv6 destination from an
/// IPv4 socket) have no mapping that could make them work, so they keep
/// surfacing as the ordinary send error.
pub fn map_dest_for(dest: SocketAddr, local: SocketAddr) -> SocketAddr {
    match (dest, local.ip()) {
        (SocketAddr::V4(v4), IpAddr::V6(_)) => {
            SocketAddr::new(IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port())
        }
        _ => dest,
    }
}

/// Canonicalize a source address observed on a socket.
///
/// A dual-stack IPv6 socket reports IPv4 peers v4-mapped (`::ffff:192.0.2.1`
/// instead of `192.0.2.1`). Unmap them so the peer table, rate limiters,
/// logs, and connection links carry the real IPv4 address rather than an
/// alias of it — otherwise one peer would hold two identities and a mapped
/// address could end up in a link that expects canonical form.
pub fn unmap_source(from: SocketAddr) -> SocketAddr {
    match from {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
            None => from,
        },
        SocketAddr::V4(_) => from,
    }
}

/// Pick from `candidates` an address whose family matches `preferred`.
///
/// `preferred` is this node's own listen address: resolving the far side to
/// the family the local socket actually dials avoids choosing an unreachable
/// candidate when a name publishes both `A` and `AAAA`. With `preferred =
/// None` (no configured family) IPv4 wins, which is the historical behaviour
/// and works on every dual-stack host. Candidates of the *other* family stay
/// usable: when nothing matches the preference the first candidate is
/// returned instead of failing, so an IPv6-only seed still resolves for an
/// IPv4-preferring caller (and vice versa).
pub fn prefer_family(
    candidates: impl IntoIterator<Item = SocketAddr>,
    preferred: Option<IpAddr>,
) -> Option<SocketAddr> {
    let want_v6 = matches!(preferred, Some(IpAddr::V6(_)));
    let candidates: Vec<SocketAddr> = candidates.into_iter().collect();
    candidates
        .iter()
        .find(|addr| addr.is_ipv6() == want_v6)
        .copied()
        .or_else(|| candidates.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(ip: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3])), port)
    }

    fn v6(ip: Ipv6Addr, port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(ip), port)
    }

    #[test]
    fn ipv4_destination_is_mapped_for_an_ipv6_socket() {
        let dest = v4([192, 0, 2, 7], 7921);
        let local = v6(Ipv6Addr::UNSPECIFIED, 7921);
        let mapped = Ipv4Addr::new(192, 0, 2, 7).to_ipv6_mapped();
        assert_eq!(map_dest_for(dest, local), v6(mapped, 7921));
        // The mapping must round-trip through unmap_source unchanged.
        assert_eq!(unmap_source(map_dest_for(dest, local)), dest);
    }

    #[test]
    fn same_family_destinations_pass_through() {
        let dest = v4([198, 51, 100, 9], 1);
        assert_eq!(map_dest_for(dest, v4([0, 0, 0, 0], 0)), dest);
        let dest6 = v6(Ipv6Addr::LOCALHOST, 2);
        assert_eq!(map_dest_for(dest6, v6(Ipv6Addr::UNSPECIFIED, 0)), dest6);
        // IPv6 from an IPv4 socket: nothing to map, the address is left as-is.
        assert_eq!(map_dest_for(dest6, v4([0, 0, 0, 0], 0)), dest6);
    }

    #[test]
    fn v4_mapped_sources_are_canonicalized() {
        let mapped = v6(Ipv4Addr::new(203, 0, 113, 1).to_ipv6_mapped(), 443);
        assert_eq!(unmap_source(mapped), v4([203, 0, 113, 1], 443));
        // Real IPv6 and real IPv4 sources are already canonical.
        assert_eq!(
            unmap_source(v6(Ipv6Addr::LOCALHOST, 1)),
            v6(Ipv6Addr::LOCALHOST, 1)
        );
        assert_eq!(unmap_source(v4([127, 0, 0, 1], 1)), v4([127, 0, 0, 1], 1));
    }

    #[test]
    fn prefer_family_follows_the_local_family_then_falls_back() {
        let candidates = [v6(Ipv6Addr::LOCALHOST, 1), v4([127, 0, 0, 1], 2)];
        // IPv6 listen address → the v6 candidate wins.
        assert_eq!(
            prefer_family(candidates, Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED))),
            Some(v6(Ipv6Addr::LOCALHOST, 1))
        );
        // IPv4 (and no) preference → the v4 candidate wins, matching the
        // pre-IPv6 behaviour.
        assert_eq!(
            prefer_family(candidates, Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED))),
            Some(v4([127, 0, 0, 1], 2))
        );
        assert_eq!(prefer_family(candidates, None), Some(v4([127, 0, 0, 1], 2)));
        // No candidate of the preferred family: fall back instead of failing.
        assert_eq!(
            prefer_family(
                [v4([127, 0, 0, 1], 2)],
                Some(IpAddr::V6(Ipv6Addr::LOCALHOST))
            ),
            Some(v4([127, 0, 0, 1], 2))
        );
        assert_eq!(prefer_family(None, None), None);
    }
}
