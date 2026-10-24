//! Two-process public-network smoke test.
//!
//! The other integration tests run nodes in-process. This one launches two
//! real `pwb-node` binaries with separate data and identity directories, waits
//! for the first to print its connection link, and has the second join through
//! `--bootstrap`. It catches wiring mistakes that only appear in the shipped
//! CLI (argument parsing, identity/data separation, startup ordering) without
//! requiring STUN or an internet connection.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
            .arg("--no-join")
            .args(extra_args)
            .env_remove("PWB_BOOTSTRAP")
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

    /// Wait for the process to exit, up to `timeout`.
    fn wait_for_exit(&mut self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll pwb-node") {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait for a line containing `needle`, up to `timeout`.
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
        "pwb-public-smoke-{}-{unique}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

#[test]
fn join_required_fails_when_no_seed_answers() {
    let dir = temp_dir("join-required");
    let generated = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs();
    let seed_key = "ab".repeat(32);
    // A syntactically valid descriptor whose only seed is a closed local port.
    let descriptor = format!(
        concat!(
            "{{\"version\":1,\"generated_at\":{},\"expires_at\":null,",
            "\"seeds\":[{{\"address\":\"127.0.0.1:1\",\"public_key\":\"{}\"}}],",
            "\"trust_anchors\":[],\"signer\":null,\"signature\":null}}"
        ),
        generated, seed_key
    );
    let join_path = dir.join("join.json");
    std::fs::write(&join_path, descriptor).expect("write join descriptor");

    let mut node = NodeProcess::spawn(
        &dir,
        &[
            "--join".to_string(),
            join_path.display().to_string(),
            "--join-required".to_string(),
        ],
    );
    let status = node
        .wait_for_exit(Duration::from_secs(30))
        .expect("--join-required must not run standalone when no seed answers");
    assert_eq!(status.code(), Some(1));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refused_claim_keeps_the_node_serving() {
    // A fresh identity has no credits, so `--claim` is refused. The node was
    // asked to serve and to own a name; it must not tear down the thing that
    // earns the credits. It keeps running and retries the claim in place.
    let dir = temp_dir("claim-retry");
    let site = dir.join("site");
    std::fs::create_dir_all(&site).expect("create site dir");
    std::fs::write(
        site.join("index.pml"),
        "<plain><head><title>Claim Retry</title></head>\
         <view><text class=\"body\">Keep serving while the balance grows.</text></view></plain>",
    )
    .expect("write index.pml");

    let mut node = NodeProcess::spawn(
        &dir,
        &[
            "--host".to_string(),
            site.display().to_string(),
            "--claim".to_string(),
            "retry.pwb".to_string(),
        ],
    );

    let refused = node
        .wait_for_line("[Claim] Refused for now:", Duration::from_secs(30))
        .expect("a fresh node cannot pay for its first claim");
    assert!(
        refused.contains("need 10 credits"),
        "refusal names the price: {refused}"
    );
    node.wait_for_line("keeps serving and retries", Duration::from_secs(5))
        .expect("the node explains that it keeps serving and retries");

    // The process must still be alive after the refusal: a claim that only
    // needs earned credits is deferred, not fatal.
    assert!(
        node.wait_for_exit(Duration::from_millis(500)).is_none(),
        "a refused claim must not stop the daemon"
    );

    drop(node);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_processes_join_over_a_bootstrap_link() {
    let dir_a = temp_dir("a");
    let dir_b = temp_dir("b");

    let node_a = NodeProcess::spawn(&dir_a, &[]);
    let link_line = node_a
        .wait_for_line("plain://nodes.plain?connect=", Duration::from_secs(30))
        .expect("first node prints a connection link");
    let link = link_line
        .split("plain://nodes.plain?connect=")
        .nth(1)
        .expect("connection link present")
        .trim()
        .to_string();
    assert!(
        link.contains('/'),
        "connection link carries an address and key: {link}"
    );

    let bootstrap = vec!["--bootstrap".to_string(), link];
    let node_b = NodeProcess::spawn(&dir_b, &bootstrap);
    let joined = node_b
        .wait_for_line("Joined via 1 bootstrap peer(s)", Duration::from_secs(20))
        .expect("second node joins the first over UDP");
    assert!(joined.contains("Joined via 1"));

    drop(node_b);
    drop(node_a);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}
