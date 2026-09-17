//! Live per-run pipeline progress.
//!
//! `/chat` and `/agents` do all their retrieval, planning and DB work *before*
//! the SSE generator opens (see the `ChatData`/`AgentData` doc comments) — the
//! generator cannot borrow `&AppState` across a yield, so stage events cannot be
//! interleaved into the answer stream itself. Without a second channel the client
//! sees a bare spinner for the whole opaque part of the request.
//!
//! So the client mints a `run_id`, subscribes to `GET /runs/<run_id>/progress`,
//! and posts the same id with the question. Every `RequestTrace` stage publishes
//! here as it starts and finishes, which is what the activity strip renders.
//!
//! Process-local and best-effort: a dropped event only costs a strip update, so
//! nothing here blocks or fails a request. Payloads carry stage names and counts
//! only — never question text, row values, or any other PHI.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::broadcast;

/// Events replayed to a subscriber that connects after the run started. Deep
/// enough for the longest pipeline (rewrite → embed → 2 searches → rrf → rerank
/// → gate → prompt → generate, start+end each) with room to spare.
const BACKLOG_CAP: usize = 64;

/// Broadcast buffer per run. Subscribers that fall this far behind get a lagged
/// receiver; the strip simply resumes from the next event.
const CHANNEL_CAP: usize = 64;

/// A run whose client never connected (or vanished mid-flight) is swept this
/// long after its first event, so an abandoned id cannot leak the map.
const RUN_TTL: Duration = Duration::from_secs(600);

/// Hard ceiling on tracked runs. Subscribing creates an entry, so this bounds a
/// client that opens progress streams for ids it never posts. Far above any
/// realistic number of in-flight answers on a single on-prem host.
const MAX_RUNS: usize = 512;

/// One pipeline transition. `status` is `"start"`, `"end"`, or `"done"` (the
/// terminal marker that closes the progress stream).
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    /// Stable stage key, e.g. `"search_vector"`. Matches `telemetry::Stage::name`.
    pub stage: &'static str,
    /// Human-readable label for the UI, e.g. `"Searching by meaning"`.
    pub label: &'static str,
    /// `"start"` | `"end"` | `"done"`.
    pub status: &'static str,
    /// Wall time for the stage; present on `"end"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    /// Optional PHI-free detail, e.g. `"6 of 30 passages kept"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Monotonic per-run sequence, so a client can dedupe a replayed backlog
    /// against live events.
    pub seq: u64,
}

impl ProgressEvent {
    pub fn new(stage: &'static str, label: &'static str, status: &'static str) -> Self {
        Self {
            stage,
            label,
            status,
            ms: None,
            detail: None,
            seq: 0,
        }
    }

    pub fn with_ms(mut self, ms: u64) -> Self {
        self.ms = Some(ms);
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug)]
struct RunChannel {
    tx: broadcast::Sender<ProgressEvent>,
    /// Replayed to late subscribers — the POST usually beats the SSE connect.
    backlog: Vec<ProgressEvent>,
    seq: u64,
    created: Instant,
}

/// Process-local fan-out of stage events, keyed by client-supplied run id.
#[derive(Clone, Debug, Default)]
pub struct RunProgressHub {
    runs: Arc<Mutex<HashMap<String, RunChannel>>>,
}

impl RunProgressHub {
    /// Record and fan out one stage transition. Creates the run's channel if the
    /// producer got here before the subscriber, which is the common ordering.
    pub fn publish(&self, run_id: &str, mut event: ProgressEvent) {
        let Ok(mut runs) = self.runs.lock() else {
            return; // A poisoned progress lock must never fail a request.
        };
        sweep(&mut runs);
        let channel = runs
            .entry(run_id.to_string())
            .or_insert_with(|| RunChannel {
                tx: broadcast::channel(CHANNEL_CAP).0,
                backlog: Vec::new(),
                seq: 0,
                created: Instant::now(),
            });
        channel.seq += 1;
        event.seq = channel.seq;
        if channel.backlog.len() == BACKLOG_CAP {
            channel.backlog.remove(0);
        }
        channel.backlog.push(event.clone());
        let _ = channel.tx.send(event); // No subscriber yet is normal.
    }

    /// Everything already emitted for this run, plus a live receiver.
    pub fn subscribe(
        &self,
        run_id: &str,
    ) -> (Vec<ProgressEvent>, broadcast::Receiver<ProgressEvent>) {
        let Ok(mut runs) = self.runs.lock() else {
            return (Vec::new(), broadcast::channel(1).0.subscribe());
        };
        sweep(&mut runs);
        let channel = runs
            .entry(run_id.to_string())
            .or_insert_with(|| RunChannel {
                tx: broadcast::channel(CHANNEL_CAP).0,
                backlog: Vec::new(),
                seq: 0,
                created: Instant::now(),
            });
        (channel.backlog.clone(), channel.tx.subscribe())
    }

    pub fn remove(&self, run_id: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(run_id);
        }
    }
}

fn sweep(runs: &mut HashMap<String, RunChannel>) {
    runs.retain(|_, channel| channel.created.elapsed() < RUN_TTL);
    while runs.len() >= MAX_RUNS {
        // Oldest first: a run that has been open longest is the likeliest to be
        // abandoned, and dropping it only ends its (already stale) strip.
        let Some(oldest) = runs
            .iter()
            .min_by_key(|(_, channel)| channel.created)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        runs.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn late_subscriber_replays_the_backlog() {
        let hub = RunProgressHub::default();
        hub.publish("run-1", ProgressEvent::new("route", "Routing", "start"));
        hub.publish(
            "run-1",
            ProgressEvent::new("route", "Routing", "end").with_ms(3),
        );

        let (backlog, mut live) = hub.subscribe("run-1");
        assert_eq!(backlog.len(), 2);
        assert_eq!(backlog[0].seq, 1);
        assert_eq!(backlog[1].ms, Some(3));

        hub.publish("run-1", ProgressEvent::new("embed", "Embedding", "start"));
        let next = live.recv().await.expect("live event");
        assert_eq!(next.stage, "embed");
        assert_eq!(next.seq, 3);
    }

    #[test]
    fn runs_are_independent_and_removable() {
        let hub = RunProgressHub::default();
        hub.publish("a", ProgressEvent::new("route", "Routing", "start"));
        hub.publish("b", ProgressEvent::new("plan", "Planning", "start"));
        assert_eq!(hub.subscribe("a").0.len(), 1);
        hub.remove("a");
        assert_eq!(hub.subscribe("a").0.len(), 0);
        assert_eq!(hub.subscribe("b").0.len(), 1);
    }
}
