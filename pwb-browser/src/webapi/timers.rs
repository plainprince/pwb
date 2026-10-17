//! Main-thread timer registry backing `setTimeout`/`setInterval`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

thread_local! {
    static TIMERS: RefCell<HashMap<u64, TimerEntry>> = RefCell::new(HashMap::new());
}

struct TimerEntry {
    interval_ms: Option<f64>,
    fire_at: Instant,
}

/// Register a timeout (`interval == false`) or interval. The JS callback lives
/// in the `__pwb_timers` global registry; here we only track the deadline.
pub fn register_timer(id: u64, ms: f64, interval: bool) {
    let ms = ms.max(0.0);
    let delay = Duration::from_millis(ms as u64);
    TIMERS.with(|t| {
        t.borrow_mut().insert(
            id,
            TimerEntry {
                interval_ms: if interval { Some(ms) } else { None },
                fire_at: Instant::now() + delay,
            },
        );
    });
}

pub fn clear_timer(id: u64) {
    TIMERS.with(|t| {
        t.borrow_mut().remove(&id);
    });
}

/// Pop every timer that is due; returns `(id, interval_ms)`.
pub fn pop_due_timers() -> Vec<(u64, Option<f64>)> {
    let now = Instant::now();
    TIMERS.with(|t| {
        let mut timers = t.borrow_mut();
        let mut due = Vec::new();
        timers.retain(|id, entry| {
            if entry.fire_at <= now {
                due.push((*id, entry.interval_ms));
                false
            } else {
                true
            }
        });
        due
    })
}

/// Re-arm an interval timer after it fired.
pub fn reschedule_timer(id: u64, ms: f64) {
    register_timer(id, ms, true);
}

/// Earliest pending timer deadline, if any (used to wake the event loop).
pub fn next_timer_deadline() -> Option<Instant> {
    TIMERS.with(|t| t.borrow().values().map(|e| e.fire_at).min())
}
