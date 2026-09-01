//! In-process log hub: captures every `tracing` event and makes it streamable to
//! the desktop app so users can watch external-service activity (Foundry model
//! download/load, embedding-model load, ingestion, index creation, errors) without
//! a terminal. See `routes/logs.rs` for the SSE endpoint that consumes it.
//!
//! Design: a `tracing_subscriber::Layer` formats each event into a `LogLine` and
//! pushes it onto a broadcast channel plus a small replay ring. A client that
//! connects mid-operation first receives the ring (recent history), then live
//! lines. Snapshot + subscribe happen under one lock so no line is lost between
//! them; the rare duplicate that a concurrent push can cause is deduped by `seq`
//! on the client.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use regex::Regex;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Matches ANSI SGR color/reset sequences (e.g. `\x1b[32m`, `\x1b[0m`). Rocket's
/// own request-logging tracing calls (`Matched: (name) GET /path`) embed these
/// directly in the event's message text for its colored terminal output — they
/// aren't added by `tracing_subscriber`'s fmt layer, so they survive into any
/// event data a `Layer` reads, this one included. Strip them here so the app's
/// plain-text log viewer shows readable text instead of raw escape codes; the
/// terminal's own `fmt::layer()` is unaffected (it reads the same raw event
/// independently and still renders the color).
static ANSI_SGR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());

/// How many recent lines to retain for replay to a freshly-connected client.
const RING_CAP: usize = 300;
/// Broadcast backlog; a slow consumer beyond this is told how many it missed.
const CHANNEL_CAP: usize = 512;

/// Monotonic id so clients can dedup (ring vs. live overlap) and order lines.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// One formatted log event, as streamed to the app.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    /// Monotonic sequence number for client-side dedup/ordering.
    pub seq: u64,
    /// RFC3339 millisecond timestamp.
    pub ts: String,
    /// Log level: TRACE | DEBUG | INFO | WARN | ERROR.
    pub level: String,
    /// Event target — the emitting module path (e.g. `onprem_server::embed`),
    /// which the UI uses to route lines to the right per-page panel.
    pub target: String,
    /// The event message plus any `key=value` fields, space-joined.
    pub message: String,
}

/// Broadcast + replay ring behind [`hub`].
pub struct LogHub {
    tx: broadcast::Sender<LogLine>,
    ring: Mutex<VecDeque<LogLine>>,
}

impl LogHub {
    fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAP);
        LogHub {
            tx,
            ring: Mutex::new(VecDeque::with_capacity(RING_CAP)),
        }
    }

    /// Record a line: append to the ring (evicting the oldest past `RING_CAP`),
    /// then broadcast. A send with no live subscribers is fine — the ring keeps it.
    fn push(&self, line: LogLine) {
        {
            let mut ring = self.ring.lock().expect("log ring poisoned");
            if ring.len() == RING_CAP {
                ring.pop_front();
            }
            ring.push_back(line.clone());
        }
        let _ = self.tx.send(line);
    }

    /// A recent-history snapshot plus a live receiver, taken together under the
    /// ring lock so no line falls between the two.
    pub fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
        let ring = self.ring.lock().expect("log ring poisoned");
        let rx = self.tx.subscribe();
        (ring.iter().cloned().collect(), rx)
    }
}

/// The process-global log hub.
pub fn hub() -> &'static LogHub {
    static HUB: OnceLock<LogHub> = OnceLock::new();
    HUB.get_or_init(LogHub::new)
}

/// `tracing` layer that funnels every event into [`hub`]. Registered alongside the
/// normal `fmt` layer in `main.rs`, so terminal output is unchanged.
pub struct BroadcastLayer;

impl<S: Subscriber> Layer<S> for BroadcastLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let line = LogLine {
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level: meta.level().as_str().to_string(),
            target: meta.target().to_string(),
            message: ANSI_SGR.replace_all(&visitor.finish(), "").into_owned(),
        };
        hub().push(line);
    }
}

/// Collects the `message` field and any structured `key=value` fields into a
/// single human-readable string. All typed `record_*` methods fall back to
/// `record_debug`, so implementing that one captures every field kind.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields.join(" ")
        } else {
            format!("{} {}", self.message, self.fields.join(" "))
        }
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }
}
