//! Structured logging and process metrics for public deployments.
//!
//! Operators exposing a node to the public internet need two things the network
//! core previously did not provide: machine-readable events and counters that a
//! health endpoint can expose. Both live here without pulling in a logging
//! framework:
//!
//! * [`log_event`] writes one JSON object per line to stderr when
//!   `PWB_LOG_FORMAT=json` is set (the human-readable startup output stays
//!   unchanged otherwise).
//! * [`Metrics`] is a lock-free set of atomic counters updated by the network
//!   loops. [`Metrics::snapshot`] returns a serializable view and
//!   [`Metrics::prometheus`] renders the same numbers in the Prometheus text
//!   exposition format.
//!
//! Nothing here trusts remote input: labels are fixed strings and counters are
//! monotonic (except the process uptime gauge).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

/// A monotonically increasing counter set describing one node process.
#[derive(Debug)]
pub struct Metrics {
    started_at: Instant,
    pub datagrams_received: AtomicU64,
    pub datagrams_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub messages_received: AtomicU64,
    pub messages_rate_limited: AtomicU64,
    pub messages_replayed: AtomicU64,
    pub messages_invalid: AtomicU64,
    pub fragments_dropped: AtomicU64,
    pub onion_cells_received: AtomicU64,
    pub onion_cells_rate_limited: AtomicU64,
    pub onion_cells_replayed: AtomicU64,
    pub onion_cells_relayed: AtomicU64,
    pub onion_cover_cells_sent: AtomicU64,
    pub onion_requests_delivered: AtomicU64,
    pub peer_dials: AtomicU64,
    pub peer_dial_failures: AtomicU64,
    pub send_queue_drops: AtomicU64,
    pub relay_messages_sent: AtomicU64,
    pub relay_messages_delivered: AtomicU64,
    pub hole_punches: AtomicU64,
    pub hole_punch_failures: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            datagrams_received: AtomicU64::new(0),
            datagrams_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            messages_received: AtomicU64::new(0),
            messages_rate_limited: AtomicU64::new(0),
            messages_replayed: AtomicU64::new(0),
            messages_invalid: AtomicU64::new(0),
            fragments_dropped: AtomicU64::new(0),
            onion_cells_received: AtomicU64::new(0),
            onion_cells_rate_limited: AtomicU64::new(0),
            onion_cells_replayed: AtomicU64::new(0),
            onion_cells_relayed: AtomicU64::new(0),
            onion_cover_cells_sent: AtomicU64::new(0),
            onion_requests_delivered: AtomicU64::new(0),
            peer_dials: AtomicU64::new(0),
            peer_dial_failures: AtomicU64::new(0),
            send_queue_drops: AtomicU64::new(0),
            relay_messages_sent: AtomicU64::new(0),
            relay_messages_delivered: AtomicU64::new(0),
            hole_punches: AtomicU64::new(0),
            hole_punch_failures: AtomicU64::new(0),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    fn bump(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }

    pub fn record_datagram_received(&self, bytes: usize) {
        Self::bump(&self.datagrams_received, 1);
        Self::bump(&self.bytes_received, bytes as u64);
    }

    pub fn record_datagram_sent(&self, bytes: usize) {
        Self::bump(&self.datagrams_sent, 1);
        Self::bump(&self.bytes_sent, bytes as u64);
    }

    pub fn record_message_received(&self) {
        Self::bump(&self.messages_received, 1);
    }

    pub fn record_message_rate_limited(&self) {
        Self::bump(&self.messages_rate_limited, 1);
    }

    pub fn record_replay(&self) {
        Self::bump(&self.messages_replayed, 1);
    }

    pub fn record_invalid(&self) {
        Self::bump(&self.messages_invalid, 1);
    }

    pub fn record_fragment_dropped(&self) {
        Self::bump(&self.fragments_dropped, 1);
    }

    pub fn record_onion_cell(&self) {
        Self::bump(&self.onion_cells_received, 1);
    }

    pub fn record_onion_rate_limited(&self) {
        Self::bump(&self.onion_cells_rate_limited, 1);
    }

    pub fn record_onion_replay(&self) {
        Self::bump(&self.onion_cells_replayed, 1);
    }

    pub fn record_onion_relayed(&self) {
        Self::bump(&self.onion_cells_relayed, 1);
    }

    pub fn record_cover_cell_sent(&self) {
        Self::bump(&self.onion_cover_cells_sent, 1);
    }

    pub fn record_onion_request(&self) {
        Self::bump(&self.onion_requests_delivered, 1);
    }

    pub fn record_peer_dial(&self, success: bool) {
        Self::bump(&self.peer_dials, 1);
        if !success {
            Self::bump(&self.peer_dial_failures, 1);
        }
    }

    pub fn record_send_queue_drop(&self) {
        Self::bump(&self.send_queue_drops, 1);
    }

    pub fn record_relay_sent(&self) {
        Self::bump(&self.relay_messages_sent, 1);
    }

    pub fn record_relay_delivered(&self) {
        Self::bump(&self.relay_messages_delivered, 1);
    }

    pub fn record_hole_punch(&self, success: bool) {
        Self::bump(&self.hole_punches, 1);
        if !success {
            Self::bump(&self.hole_punch_failures, 1);
        }
    }

    /// A stable, serializable view of every counter plus process uptime.
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            uptime_secs: self.uptime_secs(),
            datagrams_received: self.datagrams_received.load(Ordering::Relaxed),
            datagrams_sent: self.datagrams_sent.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            messages_received: self.messages_received.load(Ordering::Relaxed),
            messages_rate_limited: self.messages_rate_limited.load(Ordering::Relaxed),
            messages_replayed: self.messages_replayed.load(Ordering::Relaxed),
            messages_invalid: self.messages_invalid.load(Ordering::Relaxed),
            fragments_dropped: self.fragments_dropped.load(Ordering::Relaxed),
            onion_cells_received: self.onion_cells_received.load(Ordering::Relaxed),
            onion_cells_rate_limited: self.onion_cells_rate_limited.load(Ordering::Relaxed),
            onion_cells_replayed: self.onion_cells_replayed.load(Ordering::Relaxed),
            onion_cells_relayed: self.onion_cells_relayed.load(Ordering::Relaxed),
            onion_cover_cells_sent: self.onion_cover_cells_sent.load(Ordering::Relaxed),
            onion_requests_delivered: self.onion_requests_delivered.load(Ordering::Relaxed),
            peer_dials: self.peer_dials.load(Ordering::Relaxed),
            peer_dial_failures: self.peer_dial_failures.load(Ordering::Relaxed),
            send_queue_drops: self.send_queue_drops.load(Ordering::Relaxed),
            relay_messages_sent: self.relay_messages_sent.load(Ordering::Relaxed),
            relay_messages_delivered: self.relay_messages_delivered.load(Ordering::Relaxed),
            hole_punches: self.hole_punches.load(Ordering::Relaxed),
            hole_punch_failures: self.hole_punch_failures.load(Ordering::Relaxed),
        }
    }

    /// Render the counters in the Prometheus text exposition format (version
    /// 0.0.4). All series share the `pwb_` prefix.
    pub fn prometheus(&self) -> String {
        let snapshot = self.snapshot();
        let gauges = [("pwb_uptime_seconds", snapshot.uptime_secs)];
        let counters = [
            ("pwb_datagrams_received_total", snapshot.datagrams_received),
            ("pwb_datagrams_sent_total", snapshot.datagrams_sent),
            ("pwb_bytes_received_total", snapshot.bytes_received),
            ("pwb_bytes_sent_total", snapshot.bytes_sent),
            ("pwb_messages_received_total", snapshot.messages_received),
            (
                "pwb_messages_rate_limited_total",
                snapshot.messages_rate_limited,
            ),
            ("pwb_messages_replayed_total", snapshot.messages_replayed),
            ("pwb_messages_invalid_total", snapshot.messages_invalid),
            ("pwb_fragments_dropped_total", snapshot.fragments_dropped),
            (
                "pwb_onion_cells_received_total",
                snapshot.onion_cells_received,
            ),
            (
                "pwb_onion_cells_rate_limited_total",
                snapshot.onion_cells_rate_limited,
            ),
            (
                "pwb_onion_cells_replayed_total",
                snapshot.onion_cells_replayed,
            ),
            (
                "pwb_onion_cells_relayed_total",
                snapshot.onion_cells_relayed,
            ),
            (
                "pwb_onion_cover_cells_sent_total",
                snapshot.onion_cover_cells_sent,
            ),
            (
                "pwb_onion_requests_delivered_total",
                snapshot.onion_requests_delivered,
            ),
            ("pwb_peer_dials_total", snapshot.peer_dials),
            ("pwb_peer_dial_failures_total", snapshot.peer_dial_failures),
            ("pwb_send_queue_drops_total", snapshot.send_queue_drops),
            (
                "pwb_relay_messages_sent_total",
                snapshot.relay_messages_sent,
            ),
            (
                "pwb_relay_messages_delivered_total",
                snapshot.relay_messages_delivered,
            ),
            ("pwb_hole_punches_total", snapshot.hole_punches),
            (
                "pwb_hole_punch_failures_total",
                snapshot.hole_punch_failures,
            ),
        ];
        let mut out = String::new();
        for (name, value) in gauges {
            out.push_str(&format!("# TYPE {name} gauge\n{name} {value}\n"));
        }
        for (name, value) in counters {
            out.push_str(&format!("# TYPE {name} counter\n{name} {value}\n"));
        }
        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializable metrics view returned by `/metrics` and `/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetricsSnapshot {
    pub uptime_secs: u64,
    pub datagrams_received: u64,
    pub datagrams_sent: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
    pub messages_received: u64,
    pub messages_rate_limited: u64,
    pub messages_replayed: u64,
    pub messages_invalid: u64,
    pub fragments_dropped: u64,
    pub onion_cells_received: u64,
    pub onion_cells_rate_limited: u64,
    pub onion_cells_replayed: u64,
    pub onion_cells_relayed: u64,
    pub onion_cover_cells_sent: u64,
    pub onion_requests_delivered: u64,
    pub peer_dials: u64,
    pub peer_dial_failures: u64,
    pub send_queue_drops: u64,
    pub relay_messages_sent: u64,
    pub relay_messages_delivered: u64,
    pub hole_punches: u64,
    pub hole_punch_failures: u64,
}

/// Whether structured JSON logging was requested for this process.
pub fn json_logging_enabled() -> bool {
    matches!(
        std::env::var("PWB_LOG_FORMAT").as_deref(),
        Ok("json") | Ok("JSON")
    )
}

/// Emit one machine-readable event.
///
/// With `PWB_LOG_FORMAT=json` the event is a single JSON object on stderr;
/// otherwise it is rendered as `[event] key=value …` on the same stream so
/// existing human-facing output is unaffected. `fields` must be a JSON object
/// (non-object values are wrapped under `value`).
pub fn log_event(level: &str, event: &str, fields: Value) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let fields = match fields {
        Value::Object(map) => Value::Object(map),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            Value::Object(map)
        }
    };
    if json_logging_enabled() {
        let mut record = serde_json::Map::new();
        record.insert("ts".to_string(), Value::from(timestamp));
        record.insert("level".to_string(), Value::from(level));
        record.insert("event".to_string(), Value::from(event));
        if let Value::Object(map) = fields {
            for (key, value) in map {
                record.insert(key, value);
            }
        }
        eprintln!("{}", Value::Object(record));
    } else {
        let mut line = format!("[{event}]");
        if let Value::Object(map) = fields {
            for (key, value) in map {
                let rendered = match value {
                    Value::String(s) => s,
                    other => other.to_string(),
                };
                line.push_str(&format!(" {key}={rendered}"));
            }
        }
        eprintln!("{level}: {line}");
    }
}

/// Convenience alias so callers can share one counter set across tasks.
pub type SharedMetrics = Arc<Metrics>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_monotonic_and_render_prometheus() {
        let metrics = Metrics::new();
        metrics.record_datagram_received(120);
        metrics.record_datagram_sent(100);
        metrics.record_message_received();
        metrics.record_message_rate_limited();
        metrics.record_onion_cell();
        metrics.record_onion_relayed();
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.datagrams_received, 1);
        assert_eq!(snapshot.bytes_received, 120);
        assert_eq!(snapshot.datagrams_sent, 1);
        assert_eq!(snapshot.bytes_sent, 100);
        assert_eq!(snapshot.messages_received, 1);
        assert_eq!(snapshot.messages_rate_limited, 1);
        assert_eq!(snapshot.onion_cells_received, 1);
        assert_eq!(snapshot.onion_cells_relayed, 1);
        let text = metrics.prometheus();
        assert!(text.contains("pwb_datagrams_received_total 1"));
        assert!(text.contains("pwb_onion_cells_relayed_total 1"));
        assert!(text.contains("# TYPE pwb_uptime_seconds gauge"));
    }
}
