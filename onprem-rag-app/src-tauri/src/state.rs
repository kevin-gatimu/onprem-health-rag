//! Managed bridge state: the server base URL and (later) the JWT the bridge holds
//! on the frontend's behalf, so the token never lives in the web layer.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures_util::future::AbortHandle;

/// Mutable part of the bridge state, guarded for interior mutability.
pub struct BridgeInner {
    pub base_url: String,
    pub token: Option<String>,
}

#[derive(Default)]
struct RunRegistry {
    active: HashMap<String, AbortHandle>,
    cancelled_before_start: HashMap<String, Instant>,
}

/// Shared bridge state, managed by Tauri and accessed from commands via `State<Bridge>`.
pub struct Bridge {
    pub client: reqwest::Client,
    pub inner: Mutex<BridgeInner>,
    /// Set while the `/logs/stream` relay is running, so it starts at most once per
    /// session (many log panels share the one stream). Cleared when the stream ends.
    pub log_streaming: AtomicBool,
    active_runs: Mutex<RunRegistry>,
}

impl Bridge {
    pub fn new(base_url: String) -> Self {
        Bridge {
            client: reqwest::Client::new(),
            inner: Mutex::new(BridgeInner {
                base_url,
                token: None,
            }),
            log_streaming: AtomicBool::new(false),
            active_runs: Mutex::new(RunRegistry::default()),
        }
    }

    /// Snapshot the current base URL without holding the lock across an await point.
    pub fn base_url(&self) -> String {
        self.inner
            .lock()
            .expect("bridge lock poisoned")
            .base_url
            .clone()
    }

    /// Snapshot the stored JWT, if the user is logged in.
    pub fn token(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("bridge lock poisoned")
            .token
            .clone()
    }

    pub fn set_token(&self, token: Option<String>) {
        self.inner.lock().expect("bridge lock poisoned").token = token;
    }

    pub fn set_base_url(&self, url: String) {
        self.inner.lock().expect("bridge lock poisoned").base_url = url;
    }

    pub fn register_run(&self, run_id: String, abort_handle: AbortHandle) {
        let mut runs = self.active_runs.lock().expect("active runs lock poisoned");
        if runs.cancelled_before_start.remove(&run_id).is_some() {
            abort_handle.abort();
        } else {
            runs.active.insert(run_id, abort_handle);
        }
    }

    pub fn remove_run(&self, run_id: &str) {
        self.active_runs
            .lock()
            .expect("active runs lock poisoned")
            .active
            .remove(run_id);
    }

    pub fn abort_run(&self, run_id: &str) {
        let mut runs = self.active_runs.lock().expect("active runs lock poisoned");
        if let Some(handle) = runs.active.remove(run_id) {
            handle.abort();
        } else {
            const CANCEL_TTL: Duration = Duration::from_secs(30);
            const MAX_PENDING_CANCELS: usize = 1024;
            runs.cancelled_before_start
                .retain(|_, created_at| created_at.elapsed() < CANCEL_TTL);
            if runs.cancelled_before_start.len() >= MAX_PENDING_CANCELS {
                if let Some(oldest) = runs
                    .cancelled_before_start
                    .iter()
                    .min_by_key(|(_, created_at)| **created_at)
                    .map(|(id, _)| id.clone())
                {
                    runs.cancelled_before_start.remove(&oldest);
                }
            }
            runs.cancelled_before_start
                .insert(run_id.to_string(), Instant::now());
        }
    }

    pub fn abort_all_runs(&self) {
        let mut runs = self.active_runs.lock().expect("active runs lock poisoned");
        for (_, handle) in runs.active.drain() {
            handle.abort();
        }
        runs.cancelled_before_start.clear();
    }

    /// Build a full URL for a server path (e.g. `/health`).
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url().trim_end_matches('/'), path)
    }
}
