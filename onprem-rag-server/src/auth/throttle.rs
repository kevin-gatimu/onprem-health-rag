//! In-memory per-IP login throttle: after repeated failures from one client IP,
//! logins from that IP are locked for an exponentially growing window. Process-local
//! (not shared across server instances) — sufficient for the on-prem single-host
//! deployment; a clustered deployment would move this to shared state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failures at or above this count begin locking.
const LOCK_THRESHOLD: u32 = 5;
/// Base lock once the threshold is crossed; doubles per extra failure, capped.
const BASE_LOCK: Duration = Duration::from_secs(30);
const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// A client's failure record is forgotten after this idle period.
const RESET_AFTER: Duration = Duration::from_secs(15 * 60);

struct Attempt {
    fails: u32,
    locked_until: Option<Instant>,
    last_seen: Instant,
}

pub struct LoginThrottle {
    inner: Mutex<HashMap<String, Attempt>>,
}

impl LoginThrottle {
    pub fn new() -> Self {
        LoginThrottle {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// If the key is currently locked, return the remaining lock duration.
    /// Also opportunistically forgets stale records.
    pub fn check(&self, key: &str) -> Option<Duration> {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        if let Some(a) = map.get(key) {
            if now.duration_since(a.last_seen) > RESET_AFTER {
                map.remove(key);
                return None;
            }
            if let Some(until) = a.locked_until {
                if until > now {
                    return Some(until - now);
                }
            }
        }
        None
    }

    /// Record a failed attempt; sets/extends the lock once past the threshold.
    pub fn record_failure(&self, key: &str) {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        let a = map.entry(key.to_string()).or_insert(Attempt {
            fails: 0,
            locked_until: None,
            last_seen: now,
        });
        if now.duration_since(a.last_seen) > RESET_AFTER {
            a.fails = 0;
            a.locked_until = None;
        }
        a.fails += 1;
        a.last_seen = now;
        if a.fails >= LOCK_THRESHOLD {
            let over = a.fails - LOCK_THRESHOLD; // 0 on first lock
            let lock = BASE_LOCK
                .checked_mul(1u32 << over.min(5))
                .unwrap_or(MAX_LOCK)
                .min(MAX_LOCK);
            a.locked_until = Some(now + lock);
        }
    }

    /// Clear a client's record after a successful login.
    pub fn record_success(&self, key: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(key);
    }
}

impl Default for LoginThrottle {
    fn default() -> Self {
        Self::new()
    }
}
