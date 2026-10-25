//! Operator wallet: attested claims and blind-voucher custody for the CLI.
//!
//! Attested claims need a $k$-of-$N$ balance certificate gathered from the
//! attestor set: the operator-pinned anchors when the node has them, the
//! deterministic neighborhood of live peers otherwise ([`attested_claim`]);
//! blind claims spend a bearer voucher withdrawn earlier
//! ([`withdraw_from_peers`]) and keep only its secret locally ([`Wallet`]).
//! Vouchers are bearer instruments, so the file is written with owner-only
//! permissions on Unix.
//!
//! The collect side must agree with the verify side
//! ([`DomainService::attestor_quorum`]): a certificate signed by the live-peer
//! neighborhood is worthless on a node (or network) that pins anchors, so the
//! wallet collects from the anchors themselves — dialling them through the
//! DHT when they are not direct peers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pwb_core::crypto::{BlindVoucher, ContentId, PublicKey, VoucherSecret};
use pwb_core::dht::NodeId;
use pwb_core::net::{IssuerBindingWire, NodeEngine};
use serde::{Deserialize, Serialize};

use crate::attestation::{collect_balance_certificate, current_epoch, withdraw_blind};
use crate::blind::{blind_claim_wire, verified_binding, BlindClaim};
use crate::domain::{ClaimError, DomainRecord, DomainService};
use crate::ledger::{select_attestors, DEFAULT_ATTESTOR_N, DEFAULT_QUORUM_K};

/// One locally held voucher plus the issuer binding needed to redeem it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredVoucher {
    /// Face value in credits (a denomination the issuer supports).
    pub amount: u64,
    /// The bearer token.
    pub token: [u8; 32],
    /// Ownership-proof challenge.
    pub challenge: [u8; 32],
    /// Ownership-proof response.
    pub response: [u8; 32],
    /// The secret that proves ownership at redemption. Bearer material.
    pub secret: [u8; 32],
    /// Attestor-signed denomination-key binding.
    pub binding: IssuerBindingWire,
}

impl StoredVoucher {
    /// Rebuild the withdrawable voucher held by this entry.
    pub fn voucher(&self) -> BlindVoucher {
        BlindVoucher {
            amount: self.amount,
            token: self.token,
            challenge: self.challenge,
            response: self.response,
        }
    }

    /// Build the private claim this voucher funds, if its binding verifies.
    pub fn claim(&self) -> Option<BlindClaim> {
        let binding = verified_binding(&self.binding)?;
        let withdrawn = crate::blind::WithdrawnVoucher {
            voucher: self.voucher(),
            secret: VoucherSecret::from_bytes(self.secret),
        };
        BlindClaim::build(&withdrawn, &binding)
    }

    /// `(attestor, epoch, denomination)` for display.
    pub fn issuer(&self) -> (PublicKey, u64, u64) {
        (
            self.binding.attestor,
            self.binding.epoch,
            self.binding.denomination,
        )
    }
}

/// A small on-disk voucher store (`vouchers.json`).
#[derive(Debug, Default)]
pub struct Wallet {
    path: PathBuf,
    vouchers: Vec<StoredVoucher>,
}

#[derive(Serialize, Deserialize)]
struct WalletFile {
    version: u32,
    vouchers: Vec<StoredVoucher>,
}

impl Wallet {
    /// Open (or create) the wallet at `path`. Unreadable files yield an empty
    /// wallet rather than losing the path for a later write.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let vouchers = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WalletFile>(&bytes).ok())
            .map(|file| file.vouchers)
            .unwrap_or_default();
        Self { path, vouchers }
    }

    /// Default wallet location inside a node data directory.
    pub fn default_path(data_dir: &Path) -> PathBuf {
        data_dir.join("vouchers.json")
    }

    /// Where this wallet persists.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Held vouchers, oldest first.
    pub fn vouchers(&self) -> &[StoredVoucher] {
        &self.vouchers
    }

    /// Total face value held.
    pub fn total(&self) -> u64 {
        self.vouchers.iter().map(|voucher| voucher.amount).sum()
    }

    /// Store a freshly withdrawn receipt (including its issuer binding).
    /// Returns the number of vouchers added.
    pub fn add_receipt(
        &mut self,
        receipt: &crate::blind::WithdrawalReceipt,
    ) -> Result<usize, String> {
        let binding = receipt
            .binding
            .as_ref()
            .ok_or_else(|| "withdrawal carried no issuer binding".to_string())?;
        let mut added = 0;
        for withdrawn in &receipt.vouchers {
            self.vouchers.push(StoredVoucher {
                amount: withdrawn.voucher.amount,
                token: withdrawn.voucher.token,
                challenge: withdrawn.voucher.challenge,
                response: withdrawn.voucher.response,
                secret: withdrawn.secret.to_bytes(),
                binding: crate::blind::binding_to_wire(binding),
            });
            added += 1;
        }
        self.save()?;
        Ok(added)
    }

    /// Index of the smallest voucher that can cover `cost`.
    pub fn select(&self, cost: u64) -> Option<usize> {
        self.vouchers
            .iter()
            .enumerate()
            .filter(|(_, voucher)| voucher.amount >= cost)
            .min_by_key(|(_, voucher)| voucher.amount)
            .map(|(index, _)| index)
    }

    /// Remove a voucher (after it has been spent).
    pub fn remove(&mut self, index: usize) -> Result<StoredVoucher, String> {
        if index >= self.vouchers.len() {
            return Err("no such voucher".to_string());
        }
        let voucher = self.vouchers.remove(index);
        self.save()?;
        Ok(voucher)
    }

    fn save(&self) -> Result<(), String> {
        let file = WalletFile {
            version: 1,
            vouchers: self.vouchers.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)
            .map_err(|e| format!("cannot encode the voucher wallet: {e}"))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create '{}': {e}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| format!("cannot write '{}': {e}", tmp.display()))?;
        restrict_permissions(&tmp)?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("cannot replace '{}': {e}", self.path.display()))?;
        Ok(())
    }
}

/// Vouchers are bearer instruments: keep the file readable only by the owner.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot restrict '{}': {e}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Attestor set for `subject` at `epoch`: the pinned anchors when the node
/// has them, otherwise the deterministic neighborhood of live peers.
///
/// The collect side must match [`DomainService::attestor_quorum`], which is
/// what verifies every funding certificate. Collecting from the live-peer
/// neighborhood while the node pins anchors would produce a certificate the
/// verifier immediately rejects (`SignerNotInNeighborhood`) — exactly the
/// state a stranger is in on a fresh bootstrap, where the only live peer is
/// the seed and the anchors are unknown keys from the descriptor.
pub fn neighborhood(domain: &DomainService, subject: &PublicKey, epoch: u64) -> Vec<PublicKey> {
    let anchors = domain.trust_anchors();
    if !anchors.is_empty() {
        return anchors.to_vec();
    }
    let mut candidates: Vec<PublicKey> = domain.peers().into_iter().map(|(key, _)| key).collect();
    candidates.push(domain.public_key());
    candidates.sort_unstable_by_key(|key| key.0);
    candidates.dedup();
    select_attestors(subject, epoch, &candidates, DEFAULT_ATTESTOR_N)
}

/// Make `attestors` dialable before a collect round.
///
/// Pinned anchors arrive as bare public keys — the descriptor carries no
/// addresses — so on a fresh bootstrap they are not in the peer table and a
/// plain `send_to` would silently reach nobody. For each unknown key, run a
/// Kademlia `find_node` walk and dial the discovered contact. The dial is
/// safe against a hostile address: the key is pinned, so only a host that
/// actually holds that identity answers the Ping/Pong probe, and
/// `dial_resilient` falls back to hole punching and relays for NATed anchors.
///
/// Anchors already in the peer table are left untouched; this only adds
/// reachability, it never removes a live peer.
async fn ensure_attestors_reachable(engine: &NodeEngine, attestors: &[PublicKey]) {
    for anchor in attestors {
        if engine
            .peer_table()
            .lock()
            .unwrap()
            .contains_key(anchor)
        {
            continue;
        }
        let target = NodeId::from_public_key(anchor);
        let contacts = engine.find_node(target, Duration::from_secs(3)).await;
        // `find_node` returns contacts nearest-first; prefer one that is
        // actually the pinned key, falling back to the nearest contact when
        // the exact node was not discovered (the dial below then verifies
        // the pinned key and rejects a wrong identity).
        let contact = contacts
            .iter()
            .find(|contact| contact.public_key == *anchor)
            .or_else(|| contacts.first());
        let Some(contact) = contact else {
            continue;
        };
        let _ = engine
            .dial_resilient(contact.address, *anchor, Duration::from_secs(2))
            .await;
    }
}

/// Gather a quorum certificate for this node's balance from its attestor set.
///
/// With pinned anchors this is the anchors themselves (dialled through the DHT
/// when they are not direct peers); without anchors it is the deterministic
/// live-peer neighborhood, which is already reachable.
pub async fn balance_certificate(
    engine: &NodeEngine,
    domain: &DomainService,
    timeout: Duration,
) -> Option<crate::ledger::QuorumCertificate> {
    let subject = domain.public_key();
    let epoch = current_epoch();
    let attestors = neighborhood(domain, &subject, epoch);
    if attestors.is_empty() {
        return None;
    }
    ensure_attestors_reachable(engine, &attestors).await;
    let need = DEFAULT_QUORUM_K.min(attestors.len()).max(1);
    collect_balance_certificate(engine, subject, epoch, &attestors, need, timeout).await
}

/// Claim `name -> target` against a fresh attested balance certificate.
///
/// The certificate is collected from the node's attestor set — the pinned
/// anchors when configured — so it is verified by the same authority that
/// collected it, including on a fresh bootstrap where the anchors were never
/// direct peers.
pub async fn attested_claim(
    engine: &NodeEngine,
    domain: &DomainService,
    name: &str,
    target: ContentId,
    timeout: Duration,
) -> Result<DomainRecord, ClaimError> {
    let subject = domain.public_key();
    let epoch = current_epoch();
    let attestors = neighborhood(domain, &subject, epoch);
    if attestors.is_empty() {
        return Err(ClaimError::Attestation(
            "no attestors are configured and no live peers can attest this balance"
                .to_string(),
        ));
    }
    ensure_attestors_reachable(engine, &attestors).await;
    let need = DEFAULT_QUORUM_K.min(attestors.len()).max(1);
    let certificate =
        collect_balance_certificate(engine, subject, epoch, &attestors, need, timeout)
            .await
            .ok_or_else(|| {
                ClaimError::Attestation(format!(
                    "{need} of {} attestors did not answer",
                    attestors.len()
                ))
            })?;
    domain.claim_attested(name, target, &certificate, &attestors)
}

/// Withdraw one voucher of `denomination` credits from the first attestor
/// that vouches a sufficient balance. Returns the receipt and the issuer key.
///
/// With pinned anchors and no explicit `--issuer`, only the anchors are asked:
/// a voucher from a non-anchor issuer is unverifiable on an anchored network,
/// because the blind-claim verifier checks the issuer binding's attestor
/// membership against the same anchor set.
pub async fn withdraw_from_peers(
    engine: &NodeEngine,
    domain: &DomainService,
    denomination: u64,
    issuer: Option<PublicKey>,
    timeout: Duration,
) -> Result<(crate::blind::WithdrawalReceipt, PublicKey), String> {
    let subject = domain.public_key();
    let epoch = current_epoch();
    let anchors = domain.trust_anchors();
    let peers: Vec<PublicKey> = match issuer {
        Some(issuer) => vec![issuer],
        None if !anchors.is_empty() => anchors.to_vec(),
        None => domain
            .peers()
            .into_iter()
            .map(|(key, _)| key)
            .filter(|key| *key != subject)
            .collect(),
    };
    if peers.is_empty() {
        return Err(if anchors.is_empty() {
            "no peers are connected, so no attestor can back a withdrawal".to_string()
        } else {
            "no pinned attestor can back a withdrawal; add --trust-anchor or --issuer".to_string()
        });
    }
    ensure_attestors_reachable(engine, &peers).await;
    let mut failures = Vec::new();
    for peer in peers {
        match withdraw_blind(engine, peer, subject, epoch, denomination, 1, timeout).await {
            Ok(receipt) => return Ok((receipt, peer)),
            Err(error) => failures.push(format!("{}: {error}", short_key(&peer))),
        }
    }
    Err(format!(
        "no attestor attested enough balance for a {denomination}-credit voucher ({})",
        failures.join("; ")
    ))
}

/// Redeem a stored voucher for `name -> target`. On success the voucher is
/// removed from the wallet (the token is now spent network-wide).
pub fn blind_claim(
    domain: &DomainService,
    wallet: &mut Wallet,
    name: &str,
    target: ContentId,
) -> Result<DomainRecord, String> {
    let cost = domain.claim_cost();
    let index = wallet
        .select(cost)
        .ok_or_else(|| format!("no voucher covers the {cost}-credit claim price"))?;
    let claim = wallet.vouchers()[index]
        .claim()
        .ok_or_else(|| "the stored voucher or its issuer binding is invalid".to_string())?;
    let record = domain
        .claim_blind(name, target, &blind_claim_wire(&claim), &[])
        .map_err(|error| error.to_string())?;
    wallet.remove(index)?;
    Ok(record)
}

/// Credits earned per service kind from the local signed ledger.
pub fn earnings_by_kind(domain: &DomainService) -> BTreeMap<String, u64> {
    let mut totals = BTreeMap::new();
    for entry in domain.ledger().entries() {
        if !entry.spent {
            *totals.entry(entry.kind).or_insert(0) += entry.amount;
        }
    }
    totals
}

/// First eight hex characters of a public key, for log lines.
pub fn short_key(key: &PublicKey) -> String {
    key.to_hex()[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainRegistry, DomainService};
    use pwb_core::crypto::Keypair;
    use pwb_core::net::{NodeConfig, PeerInfo};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    fn test_engine(keypair: &Keypair) -> NodeEngine {
        NodeEngine::new(NodeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            keypair: keypair.clone(),
            bootstrap_peers: Vec::new(),
            stun_server: None,
            max_peers: 16,
            heartbeat_interval: Duration::from_secs(30),
        })
    }

    fn domain_with_peers(
        keypair: &Keypair,
        anchors: Vec<PublicKey>,
        live_peers: &[PublicKey],
    ) -> DomainService {
        let engine = test_engine(keypair);
        for peer in live_peers {
            engine.add_peer(PeerInfo {
                public_key: *peer,
                last_seen: std::time::Instant::now(),
                address: "127.0.0.1:9999".parse::<SocketAddr>().unwrap(),
                reputation: 0,
            });
        }
        DomainService::new_with_trust(
            engine,
            Arc::new(Mutex::new(DomainRegistry::new())),
            keypair.clone(),
            0,
            None,
            anchors,
        )
    }

    /// The collect side must agree with the verify side: with pinned anchors,
    /// the wallet gathers attestations from the anchors — never from the
    /// live-peer neighborhood — so the resulting certificate is one the
    /// anchored verifier accepts. This is the stranger-on-fresh-bootstrap
    /// case: the only live peer is the seed, which is not an anchor.
    #[test]
    fn test_neighborhood_prefers_pinned_anchors_over_live_peers() {
        let owner = Keypair::generate();
        let anchors: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        let seed = Keypair::generate();

        let anchored = domain_with_peers(
            &owner,
            anchors.iter().map(|a| a.public).collect(),
            &[seed.public],
        );
        let collected = neighborhood(&anchored, &owner.public, 1);
        let mut expected: Vec<PublicKey> = anchors.iter().map(|a| a.public).collect();
        expected.sort_unstable_by_key(|key| key.0);
        assert_eq!(
            collected, expected,
            "pinned anchors are the attestor set regardless of live peers"
        );
        assert!(
            !collected.contains(&seed.public),
            "a non-anchor live peer must never be asked to attest"
        );

        // Without anchors the historical live-peer neighborhood still applies.
        let unanchored = domain_with_peers(&owner, Vec::new(), &[seed.public]);
        let collected = neighborhood(&unanchored, &owner.public, 1);
        assert!(collected.contains(&seed.public));
    }
}
