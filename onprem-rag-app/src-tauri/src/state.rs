//! Managed bridge state: the server base URL and (later) the JWT the bridge holds
//! on the frontend's behalf, so the token never lives in the web layer.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

/// Mutable part of the bridge state, guarded for interior mutability.
pub struct BridgeInner {
    pub base_url: String,
    pub token: Option<String>,
}

/// Shared bridge state, managed by Tauri and accessed from commands via `State<Bridge>`.
pub struct Bridge {
    pub client: reqwest::Client,
    pub inner: Mutex<BridgeInner>,
    /// Set while the `/logs/stream` relay is running, so it starts at most once per
    /// session (many log panels share the one stream). Cleared when the stream ends.
    pub log_streaming: AtomicBool,
}

impl Bridge {
    pub fn new(base_url: String) -> Self {
        Bridge {
            client: reqwest::Client::new(),
            inner: Mutex::new(BridgeInner { base_url, token: None }),
            log_streaming: AtomicBool::new(false),
        }
    }

    /// Snapshot the current base URL without holding the lock across an await point.
    pub fn base_url(&self) -> String {
        self.inner.lock().expect("bridge lock poisoned").base_url.clone()
    }

    /// Snapshot the stored JWT, if the user is logged in.
    pub fn token(&self) -> Option<String> {
        self.inner.lock().expect("bridge lock poisoned").token.clone()
    }

    pub fn set_token(&self, token: Option<String>) {
        self.inner.lock().expect("bridge lock poisoned").token = token;
    }

    pub fn set_base_url(&self, url: String) {
        self.inner.lock().expect("bridge lock poisoned").base_url = url;
    }

    /// Build a full URL for a server path (e.g. `/health`).
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url().trim_end_matches('/'), path)
    }
}
