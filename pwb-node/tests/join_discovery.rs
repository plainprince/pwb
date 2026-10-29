//! A stranger's first join: fetch a signed join descriptor, resolve its seed,
//! and connect without an out-of-band connection link.
//!
//! Two real `pwb-node` processes are used. The first is a rendezvous seed; the
//! second is started with only `--join <http://…>` and a fresh data directory.
//! The descriptor is signed and the client pins the signer out of band with
//! `--join-signer`, because a plain-http fetch has no transport security: an
//! unauthenticated first fetch must not be able to establish the pin. The test
//! exercises the fetch, verification, DNS/IP seed conversion, and the normal
//! Ping/Pong join path, and checks that the same fetch without a pinned signer
//! is refused.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pwb_core::crypto::Keypair;
use pwb_core::join::JoinDescriptor;
use pwb_node::join::{build_descriptor, parse_seed_spec, write_descriptor};

struct NodeProcess {
    child: Child,
    lines: Receiver<String>,
}

impl NodeProcess {
    fn spawn(data_dir: &Path, extra_args: &[String]) -> Self {
        std::fs::create_dir_all(data_dir).expect("create node data dir");
        let identity = data_dir.join("identity.json");
        let data = data_dir.join("data");
        let mut command = Command::new(env!("CARGO_BIN_EXE_pwb-node"));
        command
            .arg("--listen")
            .arg("127.0.0.1:0")
            .arg("--identity")
            .arg(&identity)
            .arg("--data")
            .arg(&data)
            .arg("--no-stun")
            .arg("--no-health")
            .args(extra_args)
            .env_remove("PWB_BOOTSTRAP")
            .env_remove("PWB_JOIN")
            .env_remove("PWB_IDENTITY")
            .env_remove("PWB_DATA")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().expect("spawn pwb-node");
        let stdout = child.stdout.take().expect("node stdout");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    fn wait_for_line(&self, needle: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) if line.contains(needle) => return Some(line),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                _ => return None,
            }
        }
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn temp_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "pwb-join-test-{}-{unique}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// Serve one descriptor body over HTTP and return the URL.
fn serve_descriptor(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind join server");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    format!("http://{addr}/join.json")
}

#[test]
fn a_stranger_joins_from_a_signed_http_descriptor() {
    let seed_dir = temp_dir("seed");
    let seed = NodeProcess::spawn(&seed_dir, &["--no-join".to_string()]);
    let link_line = seed
        .wait_for_line("plain://nodes.plain?connect=", Duration::from_secs(30))
        .expect("seed prints its link");
    let link = link_line
        .split("plain://nodes.plain?connect=")
        .nth(1)
        .expect("link payload")
        .trim()
        .to_string();
    let seed_info = parse_seed_spec(&link).expect("link parses as a join seed");

    let signer = Keypair::generate();
    let descriptor: JoinDescriptor =
        build_descriptor(vec![seed_info], Vec::new(), None, Some(&signer));
    let url = serve_descriptor(descriptor.to_json().expect("descriptor JSON"));

    // A cleartext fetch carries no transport authentication, so the signer
    // must be known before the fetch rather than pinned from it.
    let joiner_dir = temp_dir("joiner");
    let joiner = NodeProcess::spawn(
        &joiner_dir,
        &[
            "--join".to_string(),
            url.clone(),
            "--join-signer".to_string(),
            signer.public.to_hex(),
            "--require-join-signature".to_string(),
        ],
    );
    let joined = joiner
        .wait_for_line("Joined via 1 bootstrap peer(s)", Duration::from_secs(30))
        .expect("joiner reaches the published seed through --join");
    assert!(joined.contains("Joined via 1"));

    drop(joiner);

    // The same fetch with no signer pinned must not bootstrap trust: a man in
    // the middle on a plain-http fetch would otherwise choose the key this
    // node pins on first use.
    let unpinned_dir = temp_dir("joiner-unpinned");
    let mut unpinned = NodeProcess::spawn(
        &unpinned_dir,
        &[
            "--join".to_string(),
            url,
            "--require-join-signature".to_string(),
            "--join-required".to_string(),
        ],
    );
    let status = unpinned
        .wait_exit(Duration::from_secs(20))
        .expect("the unauthenticated fetch is refused before joining");
    assert!(
        !status.success(),
        "a cleartext descriptor with no pinned signer must not join"
    );
    assert!(
        pwb_node::join::load_pinned_signer(&unpinned_dir.join("data")).is_none(),
        "a cleartext first fetch is never pinned"
    );

    drop(unpinned);
    drop(seed);
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&joiner_dir);
    let _ = std::fs::remove_dir_all(&unpinned_dir);
}

#[test]
fn a_tampered_descriptor_does_not_join() {
    let seed_dir = temp_dir("seed-tamper");
    let seed = NodeProcess::spawn(&seed_dir, &["--no-join".to_string()]);
    let link_line = seed
        .wait_for_line("plain://nodes.plain?connect=", Duration::from_secs(30))
        .expect("seed prints its link");
    let link = link_line
        .split("plain://nodes.plain?connect=")
        .nth(1)
        .expect("link payload")
        .trim()
        .to_string();
    let seed_info = parse_seed_spec(&link).expect("link parses as a join seed");

    let signer = Keypair::generate();
    let descriptor = build_descriptor(vec![seed_info], Vec::new(), None, Some(&signer));
    let mut json: serde_json::Value =
        serde_json::from_slice(&descriptor.to_json().unwrap()).unwrap();
    json["seeds"][0]["address"] = serde_json::json!("127.0.0.1:9");
    let url = serve_descriptor(serde_json::to_vec(&json).unwrap());

    let joiner_dir = temp_dir("joiner-tamper");
    let mut joiner = NodeProcess::spawn(
        &joiner_dir,
        &[
            "--join".to_string(),
            url,
            "--require-join-signature".to_string(),
            "--join-required".to_string(),
        ],
    );
    let status = joiner
        .wait_exit(Duration::from_secs(20))
        .expect("the tampered descriptor is refused before joining");
    assert!(
        !status.success(),
        "the joiner must exit on a bad descriptor"
    );
    assert!(
        pwb_node::join::load_pinned_signer(&joiner_dir.join("data")).is_none(),
        "a rejected descriptor is never pinned"
    );

    drop(joiner);
    drop(seed);
    let _ = std::fs::remove_dir_all(&seed_dir);
    let _ = std::fs::remove_dir_all(&joiner_dir);
}

fn verify_join(path: &Path, signer: Option<&str>, extra: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pwb-node"));
    command
        .arg("--verify-join")
        .arg(path)
        .env_remove("PWB_JOIN_SIGNER")
        .env_remove("PWB_DATA")
        .env_remove("PWB_IDENTITY");
    if let Some(signer) = signer {
        command.arg("--join-signer").arg(signer);
    }
    command.args(extra).output().expect("run --verify-join")
}

#[test]
fn a_signed_withdrawal_round_trips_and_normal_writes_still_require_seeds() {
    let dir = temp_dir("withdraw");
    let identity = dir.join("release.json");
    let path = dir.join("join.json");

    let output = Command::new(env!("CARGO_BIN_EXE_pwb-node"))
        .arg("--identity")
        .arg(&identity)
        .arg("--write-join")
        .arg(&path)
        .arg("--withdraw-join")
        .env_remove("PWB_BOOTSTRAP")
        .env_remove("PWB_TRUST_ANCHORS")
        .env_remove("PWB_JOIN")
        .env_remove("PWB_JOIN_SIGNER")
        .output()
        .expect("write a withdrawal descriptor");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let descriptor = JoinDescriptor::parse(&std::fs::read(&path).unwrap()).expect("parses");
    assert!(descriptor.seeds.is_empty(), "a withdrawal lists no seeds");
    let signer = descriptor.signer.expect("a withdrawal is signed");
    descriptor
        .verify_signature()
        .expect("the withdrawal signature verifies");

    let verified = verify_join(&path, Some(&signer.to_hex()), &["--require-join-signature"]);
    assert!(
        verified.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        text.contains("signed withdrawal"),
        "verification should name the withdrawal: {text}"
    );

    // The guard for ordinary writes stays: a typo must not silently publish
    // an empty seed list.
    let empty = dir.join("empty.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pwb-node"))
        .arg("--identity")
        .arg(&identity)
        .arg("--write-join")
        .arg(&empty)
        .env_remove("PWB_BOOTSTRAP")
        .output()
        .expect("run a normal write without seeds");
    assert!(
        !output.status.success(),
        "a zero-seed write needs the explicit flag"
    );
    assert!(!empty.exists(), "the refused write leaves no file behind");

    // Withdrawing and naming seeds at the same time is contradictory.
    let output = Command::new(env!("CARGO_BIN_EXE_pwb-node"))
        .arg("--identity")
        .arg(&identity)
        .arg("--write-join")
        .arg(dir.join("mixed.json"))
        .arg("--withdraw-join")
        .arg("--join-seed")
        .arg(format!(
            "seed.example.com:7921/{}",
            Keypair::generate().public.to_hex()
        ))
        .output()
        .expect("run a contradictory write");
    assert!(!output.status.success());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_join_checks_a_descriptor_without_creating_an_identity() {
    let dir = temp_dir("verify");
    let signer = Keypair::generate();
    let seed = parse_seed_spec(&format!(
        "seed.example.com:7921/{}",
        Keypair::generate().public.to_hex()
    ))
    .expect("seed spec parses");
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 30 * 24 * 60 * 60;
    let descriptor = build_descriptor(vec![seed], Vec::new(), Some(expires), Some(&signer));
    let path = dir.join("join.json");
    write_descriptor(&path, &descriptor).expect("descriptor written");

    // The maintenance command must not mint an identity as a side effect.
    let identity = dir.join("identity.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pwb-node"))
        .arg("--identity")
        .arg(&identity)
        .arg("--verify-join")
        .arg(&path)
        .arg("--join-signer")
        .arg(signer.public.to_hex())
        .arg("--require-join-signature")
        .env_remove("PWB_JOIN_SIGNER")
        .output()
        .expect("run --verify-join");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Verified"), "stdout: {stdout}");
    assert!(stdout.contains("seed.example.com:7921"), "stdout: {stdout}");
    assert!(
        !identity.exists(),
        "verification must not create an identity"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_join_rejects_tampering_and_missing_signatures() {
    let dir = temp_dir("verify-reject");
    let signer = Keypair::generate();
    let seed = parse_seed_spec(&format!(
        "seed.example.com:7921/{}",
        Keypair::generate().public.to_hex()
    ))
    .expect("seed spec parses");
    let descriptor = build_descriptor(vec![seed], Vec::new(), None, Some(&signer));

    let mut json: serde_json::Value =
        serde_json::from_slice(&descriptor.to_json().unwrap()).unwrap();
    json["seeds"][0]["address"] = serde_json::json!("evil.example:7921");
    let tampered = dir.join("tampered.json");
    std::fs::write(&tampered, serde_json::to_vec(&json).unwrap()).unwrap();
    let output = verify_join(
        &tampered,
        Some(&signer.public.to_hex()),
        &["--require-join-signature"],
    );
    assert!(!output.status.success(), "a tampered descriptor must fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("signature"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let unsigned = build_descriptor(
        vec![parse_seed_spec(&format!(
            "seed.example.com:7921/{}",
            Keypair::generate().public.to_hex()
        ))
        .unwrap()],
        Vec::new(),
        None,
        None,
    );
    let unsigned_path = dir.join("unsigned.json");
    write_descriptor(&unsigned_path, &unsigned).unwrap();
    let output = verify_join(&unsigned_path, None, &["--require-join-signature"]);
    assert!(!output.status.success(), "unsigned must fail when required");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsigned"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Keep the unused import warning away while documenting that descriptors can
/// also be written to disk for an operator's publishing workflow.
#[test]
fn descriptor_writer_round_trips() {
    let dir = temp_dir("writer");
    let path = dir.join("join.json");
    let descriptor = build_descriptor(Vec::new(), Vec::new(), None, Some(&Keypair::generate()));
    write_descriptor(&path, &descriptor).unwrap();
    let parsed = JoinDescriptor::parse(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed, descriptor);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The descriptor committed to the repository is what every stranger fetches
/// by default. A typo or a hand edit there fails on the client, so keep it
/// parseable here and, once it publishes seeds, signed and unexpired.
#[test]
fn committed_join_descriptor_is_publishable() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/join.json");
    let bytes = std::fs::read(&path).expect("deploy/join.json exists");
    let descriptor = JoinDescriptor::parse(&bytes).expect("deploy/join.json parses");

    if !descriptor.seeds.is_empty() {
        assert!(
            descriptor.signer.is_some(),
            "a descriptor that publishes seeds must be signed"
        );
        descriptor
            .verify_signature()
            .expect("the committed descriptor's signature verifies");
    }
    if let Some(expires) = descriptor.expires_at {
        assert!(
            expires > pwb_core::discovery::unix_time(),
            "deploy/join.json expired at {expires}; republish it before shipping"
        );
    }
}
