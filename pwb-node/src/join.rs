//! Fetching, pinning, and publishing well-known join descriptors.
//!
//! [`pwb_core::join`] defines the document; this module adds the deployment
//! behaviour a client needs:
//!
//! - fetch a descriptor from an HTTPS URL, a local file, or an inline JSON
//!   document (bounded to [`MAX_JOIN_BYTES`]);
//! - verify its optional signature, enforcing a trust-on-first-use signer pin
//!   under `--data` so a later mirror cannot silently replace the seed list;
//! - resolve published seeds into [`BootstrapPeer`]s and merge descriptors from
//!   several sources;
//! - publish a new descriptor from the operator's seed/anchor configuration.
//!
//! A descriptor is only a rendezvous/attestor directory. Joining still proves
//! each seed with a Ping/Pong round-trip, and every subsequent message is
//! authenticated normally.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pwb_core::crypto::{Keypair, PublicKey};
use pwb_core::discovery::unix_time;
use pwb_core::join::{JoinDescriptor, JoinError, JoinSeed, MAX_JOIN_ANCHORS, MAX_JOIN_SEEDS};
use pwb_core::net::{resolve_bootstrap_peer, BootstrapPeer};

/// Default descriptor used when no explicit bootstrap or join source is given.
///
/// The file lives in the public repository so an operator can rotate seeds
/// without shipping a new binary. It may be empty until a seed is published;
/// clients then report that no public seed exists rather than inventing one.
pub const DEFAULT_JOIN_URL: &str =
    "https://raw.githubusercontent.com/plainprince/pwb/main/deploy/join.json";

/// Largest descriptor body accepted from any source.
pub const MAX_JOIN_BYTES: usize = 256 * 1024;

/// How long one descriptor fetch may take.
pub const JOIN_FETCH_TIMEOUT: Duration = Duration::from_secs(8);

/// File under the node's data directory that remembers the first signer seen.
pub const PINNED_SIGNER_FILE: &str = "join-signer.json";

/// Why a join source could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinSourceError {
    /// Network, filesystem, or HTTP failure.
    Fetch(String),
    /// The source replied 404: no descriptor has been published there yet.
    NotPublished(String),
    /// The bytes were not a valid descriptor.
    Parse(JoinError),
    /// The descriptor is old.
    Expired,
    /// A signature is required but the descriptor is unsigned.
    MissingSignature,
    /// The descriptor's signer differs from the pinned signer.
    SignerMismatch { expected: PublicKey, got: PublicKey },
    /// No source produced a usable descriptor.
    NoSource,
}

impl std::fmt::Display for JoinSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinSourceError::Fetch(e) => write!(f, "fetch failed: {e}"),
            JoinSourceError::NotPublished(url) => {
                write!(f, "no seed list is published at {url} yet (HTTP 404)")
            }
            JoinSourceError::Parse(e) => write!(f, "{e}"),
            JoinSourceError::Expired => write!(f, "join descriptor has expired"),
            JoinSourceError::MissingSignature => {
                write!(f, "join descriptor is unsigned but a signature is required")
            }
            JoinSourceError::SignerMismatch { expected, got } => write!(
                f,
                "join signer {} does not match pinned signer {}",
                got.to_hex(),
                expected.to_hex()
            ),
            JoinSourceError::NoSource => write!(f, "no usable join source"),
        }
    }
}

impl std::error::Error for JoinSourceError {}

impl From<JoinError> for JoinSourceError {
    fn from(error: JoinError) -> Self {
        match error {
            JoinError::Expired => JoinSourceError::Expired,
            other => JoinSourceError::Parse(other),
        }
    }
}

impl JoinSourceError {
    /// One line of operator guidance for a failed join source.
    ///
    /// A missing public descriptor is a normal state before a network is
    /// deployed, not a fault: say what to do instead of leaving the raw HTTP
    /// error as the last word.
    pub fn advice(&self) -> &'static str {
        match self {
            JoinSourceError::NotPublished(_) => {
                "No public seed list is published yet. Staying on LAN discovery; dial a peer yourself with --connect <link> or --bootstrap <host:port>/<key>."
            }
            JoinSourceError::Expired => {
                "The published seed list has expired. Dial a seed directly with --bootstrap <host:port>/<key>."
            }
            JoinSourceError::MissingSignature | JoinSourceError::SignerMismatch { .. } => {
                "The seed list did not pass the local signature policy. Check --join-signer and --require-join-signature."
            }
            JoinSourceError::Fetch(_) | JoinSourceError::Parse(_) | JoinSourceError::NoSource => {
                "Staying on LAN discovery; dial a peer yourself with --connect <link> or --bootstrap <host:port>/<key>."
            }
        }
    }
}

/// Fetch one descriptor. `source` may be `https://…`, `http://…`, `file://…`,
/// a filesystem path, or an inline JSON document beginning with `{`.
pub async fn fetch_descriptor(source: &str) -> Result<JoinDescriptor, JoinSourceError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(JoinSourceError::Fetch("empty join source".to_string()));
    }
    let bytes = if source.starts_with('{') {
        source.as_bytes().to_vec()
    } else if let Some(path) = source.strip_prefix("file://") {
        read_bounded(Path::new(path))?
    } else if source.starts_with("http://") || source.starts_with("https://") {
        let url = source.to_string();
        tokio::task::spawn_blocking(move || fetch_http(&url))
            .await
            .map_err(|e| JoinSourceError::Fetch(e.to_string()))??
    } else {
        read_bounded(Path::new(source))?
    };
    if bytes.len() > MAX_JOIN_BYTES {
        return Err(JoinSourceError::Fetch(format!(
            "descriptor is larger than {MAX_JOIN_BYTES} bytes"
        )));
    }
    JoinDescriptor::parse(&bytes).map_err(JoinSourceError::Parse)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, JoinSourceError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| JoinSourceError::Fetch(format!("{}: {e}", path.display())))?;
    if metadata.len() > MAX_JOIN_BYTES as u64 {
        return Err(JoinSourceError::Fetch(format!(
            "{} is larger than {MAX_JOIN_BYTES} bytes",
            path.display()
        )));
    }
    std::fs::read(path).map_err(|e| JoinSourceError::Fetch(format!("{}: {e}", path.display())))
}

fn fetch_http(url: &str) -> Result<Vec<u8>, JoinSourceError> {
    let response = ureq::builder()
        .timeout(JOIN_FETCH_TIMEOUT)
        .user_agent("pwb-join/1")
        .build()
        .get(url)
        .call()
        .map_err(|e| match e {
            // A 404 is the ordinary "nothing published yet" case, not a fault;
            // ureq's Display already carries the URL, so do not repeat it.
            ureq::Error::Status(404, _) => JoinSourceError::NotPublished(url.to_string()),
            other => JoinSourceError::Fetch(other.to_string()),
        })?;
    if response.status() != 200 {
        return Err(JoinSourceError::Fetch(format!(
            "{url}: HTTP {}",
            response.status()
        )));
    }
    if let Some(length) = response.header("Content-Length") {
        if let Ok(length) = length.parse::<usize>() {
            if length > MAX_JOIN_BYTES {
                return Err(JoinSourceError::Fetch(format!(
                    "{url}: descriptor is larger than {MAX_JOIN_BYTES} bytes"
                )));
            }
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take((MAX_JOIN_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| JoinSourceError::Fetch(format!("{url}: {e}")))?;
    if bytes.len() > MAX_JOIN_BYTES {
        return Err(JoinSourceError::Fetch(format!(
            "{url}: descriptor is larger than {MAX_JOIN_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

/// Path of the trust-on-first-use signer pin.
pub fn pinned_signer_path(data_dir: &Path) -> PathBuf {
    data_dir.join(PINNED_SIGNER_FILE)
}

/// Read the pinned join signer, if one was established on an earlier run.
pub fn load_pinned_signer(data_dir: &Path) -> Option<PublicKey> {
    let bytes = std::fs::read(pinned_signer_path(data_dir)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let hex = value.get("signer")?.as_str()?;
    let bytes = hex::decode(hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Some(PublicKey(key))
}

/// Persist the first signer seen for this data directory.
pub fn save_pinned_signer(data_dir: &Path, signer: PublicKey) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let value = serde_json::json!({
        "version": 1,
        "signer": signer.to_hex(),
    });
    let bytes = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
    crate::persist::atomic_write(&pinned_signer_path(data_dir), &bytes)
}

/// Whether a descriptor is allowed by the local policy.
///
/// A signed descriptor is always verified. If a signer is pinned (explicitly or
/// by trust-on-first-use), the descriptor must carry the same signer. An
/// unsigned descriptor is accepted only when signatures are not required.
pub fn check_descriptor(
    descriptor: &JoinDescriptor,
    pinned: Option<PublicKey>,
    require_signature: bool,
    now: u64,
) -> Result<(), JoinSourceError> {
    descriptor.ensure_current(now)?;
    descriptor.verify_signature()?;
    match (pinned, descriptor.signer) {
        (Some(expected), Some(got)) if expected != got => {
            return Err(JoinSourceError::SignerMismatch { expected, got });
        }
        (Some(_), None) => return Err(JoinSourceError::MissingSignature),
        _ => {}
    }
    if require_signature && descriptor.signer.is_none() {
        return Err(JoinSourceError::MissingSignature);
    }
    Ok(())
}

/// Fetch and merge every source, applying the signer policy.
///
/// The first signed descriptor establishes the trust-on-first-use pin when
/// `data_dir` is supplied and no explicit `pinned` key is given. Sources are
/// merged in order; seeds and anchors are deduplicated.
pub async fn discover(
    sources: &[String],
    data_dir: Option<&Path>,
    pinned: Option<PublicKey>,
    require_signature: bool,
) -> Result<JoinDescriptor, JoinSourceError> {
    if sources.is_empty() {
        return Err(JoinSourceError::NoSource);
    }
    let stored = data_dir.and_then(load_pinned_signer);
    let mut expected = pinned.or(stored);
    let now = unix_time();
    let mut merged: Option<JoinDescriptor> = None;
    let mut last_error: Option<JoinSourceError> = None;

    for source in sources {
        match fetch_descriptor(source).await {
            Ok(mut descriptor) => {
                if let Err(error) = check_descriptor(&descriptor, expected, require_signature, now)
                {
                    last_error = Some(error);
                    continue;
                }
                // An unsigned descriptor can still name seeds (TLS protects the
                // first fetch), but it must not be allowed to inject funding
                // authorities. Anchors are only honoured when the document is
                // signed and its signature verified above.
                if descriptor.signer.is_none() && !descriptor.trust_anchors.is_empty() {
                    eprintln!(
                        "[Join] Ignoring {} trust anchor(s) in an unsigned descriptor",
                        descriptor.trust_anchors.len()
                    );
                    descriptor.trust_anchors.clear();
                }
                if let (None, Some(signer), Some(data_dir)) =
                    (expected, descriptor.signer, data_dir)
                {
                    if let Err(error) = save_pinned_signer(data_dir, signer) {
                        eprintln!("[Join] Could not pin join signer: {error}");
                    } else {
                        println!("[Join] Pinned join signer {}", signer.to_hex());
                    }
                    expected = Some(signer);
                }
                match &mut merged {
                    Some(have) => have.merge(&descriptor),
                    None => merged = Some(descriptor),
                }
            }
            Err(error) => last_error = Some(error),
        }
    }

    match merged {
        Some(mut descriptor) => {
            descriptor.seeds.truncate(MAX_JOIN_SEEDS);
            descriptor.trust_anchors.truncate(MAX_JOIN_ANCHORS);
            Ok(descriptor)
        }
        None => Err(last_error.unwrap_or(JoinSourceError::NoSource)),
    }
}

/// Parse one `--join-seed` value: `<host:port>/<hex_pubkey>` or a
/// `plain://nodes.plain?connect=<host:port>/<hex>` link. Unlike
/// [`resolve_bootstrap_peer`], this keeps a DNS name instead of resolving it,
/// so a published descriptor can follow the seed's address changes.
pub fn parse_seed_spec(spec: &str) -> Option<JoinSeed> {
    let spec = spec.trim();
    let connect = spec
        .strip_prefix("plain://nodes.plain?connect=")
        .unwrap_or(spec);
    let (address, key) = connect.rsplit_once('/')?;
    if address.is_empty() || address.len() > pwb_core::join::MAX_SEED_ADDRESS_LEN {
        return None;
    }
    let bytes = hex::decode(key.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(&bytes);
    Some(JoinSeed {
        address: address.trim().to_string(),
        public_key: PublicKey(public_key),
    })
}

/// Turn descriptor seeds into dialable bootstrap peers. Unresolvable entries
/// are reported and skipped, matching `--bootstrap` behaviour.
pub async fn resolve_seeds(descriptor: &JoinDescriptor) -> Vec<BootstrapPeer> {
    let mut peers = Vec::new();
    for seed in &descriptor.seeds {
        let spec = format!("{}/{}", seed.address, seed.public_key.to_hex());
        match resolve_bootstrap_peer(&spec).await {
            Some(peer) => {
                if !peers
                    .iter()
                    .any(|have: &BootstrapPeer| have.public_key == peer.public_key)
                {
                    peers.push(peer);
                }
            }
            None => eprintln!("[Join] Ignoring unresolvable seed '{}'", seed.address),
        }
    }
    peers
}

/// Populate a descriptor from an operator's seed and anchor list, optionally
/// signing it. The seed's own link is normally one of `seeds`.
pub fn build_descriptor(
    seeds: Vec<JoinSeed>,
    trust_anchors: Vec<PublicKey>,
    expires_at: Option<u64>,
    signer: Option<&Keypair>,
) -> JoinDescriptor {
    let mut descriptor = JoinDescriptor {
        version: pwb_core::join::JOIN_VERSION,
        generated_at: unix_time(),
        expires_at,
        seeds,
        trust_anchors,
        signer: None,
        signature: None,
    };
    descriptor.trust_anchors.sort_unstable_by_key(|key| key.0);
    descriptor.trust_anchors.dedup();
    if let Some(keypair) = signer {
        descriptor.sign(keypair);
    }
    descriptor
}

/// Write a descriptor as pretty JSON, creating parent directories.
pub fn write_descriptor(path: &Path, descriptor: &JoinDescriptor) -> std::io::Result<()> {
    let bytes = descriptor
        .to_json()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwb_core::crypto::Keypair;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn descriptor() -> JoinDescriptor {
        let seed_key = Keypair::generate();
        build_descriptor(
            vec![JoinSeed {
                address: "seed.example.com:7921".to_string(),
                public_key: seed_key.public,
            }],
            vec![Keypair::generate().public],
            None,
            Some(&Keypair::generate()),
        )
    }

    #[test]
    fn check_accepts_unsigned_when_allowed_and_rejects_when_required() {
        let mut descriptor = descriptor();
        descriptor.signer = None;
        descriptor.signature = None;
        assert_eq!(check_descriptor(&descriptor, None, false, 0), Ok(()));
        assert_eq!(
            check_descriptor(&descriptor, None, true, 0),
            Err(JoinSourceError::MissingSignature)
        );
    }

    #[test]
    fn check_enforces_the_pinned_signer() {
        let descriptor = descriptor();
        let pinned = descriptor.signer.unwrap();
        assert_eq!(check_descriptor(&descriptor, Some(pinned), true, 0), Ok(()));

        let other = Keypair::generate().public;
        assert!(matches!(
            check_descriptor(&descriptor, Some(other), true, 0),
            Err(JoinSourceError::SignerMismatch { .. })
        ));
    }

    #[test]
    fn inline_and_file_sources_round_trip() {
        let descriptor = descriptor();
        let json = String::from_utf8(descriptor.to_json().unwrap()).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let inline = runtime.block_on(fetch_descriptor(&json)).unwrap();
        assert_eq!(inline, descriptor);

        let dir = std::env::temp_dir().join(format!("pwb-join-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("join.json");
        write_descriptor(&path, &descriptor).unwrap();
        let file = runtime
            .block_on(fetch_descriptor(&format!("file://{}", path.display())))
            .unwrap();
        assert_eq!(file, descriptor);
        let plain = runtime
            .block_on(fetch_descriptor(path.to_str().unwrap()))
            .unwrap();
        assert_eq!(plain, descriptor);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_source_is_fetched_and_bounded() {
        let descriptor = descriptor();
        let body = descriptor.to_json().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let fetched = runtime
            .block_on(fetch_descriptor(&format!("http://{addr}/join.json")))
            .unwrap();
        assert_eq!(fetched, descriptor);
    }

    #[test]
    fn an_unsigned_descriptor_cannot_inject_trust_anchors() {
        let dir = std::env::temp_dir().join(format!("pwb-join-unsigned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let seed = Keypair::generate();
        let anchor = Keypair::generate().public;
        let mut descriptor = build_descriptor(
            vec![JoinSeed {
                address: "seed.example.com:7921".to_string(),
                public_key: seed.public,
            }],
            vec![anchor],
            None,
            None,
        );
        descriptor.trust_anchors = vec![anchor];
        let path = dir.join("join.json");
        write_descriptor(&path, &descriptor).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let discovered = runtime
            .block_on(discover(
                &[path.to_string_lossy().to_string()],
                Some(&dir),
                None,
                false,
            ))
            .unwrap();
        assert_eq!(discovered.seeds.len(), 1);
        assert!(
            discovered.trust_anchors.is_empty(),
            "an unsigned descriptor may name seeds but not funding authorities"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_pins_the_first_signed_signer() {
        let dir = std::env::temp_dir().join(format!("pwb-join-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let descriptor = descriptor();
        let path = dir.join("join.json");
        write_descriptor(&path, &descriptor).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let discovered = runtime
            .block_on(discover(
                &[path.to_string_lossy().to_string()],
                Some(&dir),
                None,
                true,
            ))
            .unwrap();
        assert_eq!(discovered, descriptor);
        assert_eq!(load_pinned_signer(&dir), descriptor.signer);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
