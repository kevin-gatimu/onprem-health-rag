//! PHI-safe request timing and summary instrumentation for RAG endpoints.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use serde::Serialize;
use std::time::{Duration, Instant};

use tracing::Instrument;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Route,
    History,
    RewriteExpand,
    Embed,
    SearchVector,
    SearchText,
    Rrf,
    Rerank,
    Gate,
    Prompt,
    Ttft,
    Generate,
    Persist,
    Plan,
    Validate,
    Execute,
    Narrate,
}

impl Stage {
    const fn name(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::History => "history",
            Self::RewriteExpand => "rewrite_expand",
            Self::Embed => "embed",
            Self::SearchVector => "search_vector",
            Self::SearchText => "search_text",
            Self::Rrf => "rrf",
            Self::Rerank => "rerank",
            Self::Gate => "gate",
            Self::Prompt => "prompt",
            Self::Ttft => "ttft",
            Self::Generate => "generate",
            Self::Persist => "persist",
            Self::Plan => "plan",
            Self::Validate => "validate",
            Self::Execute => "execute",
            Self::Narrate => "narrate",
        }
    }

    /// Short, human-readable description of what this stage is doing, shown live
    /// in the client activity strip. Plain language on purpose — the strip is for
    /// clinicians watching an answer being assembled, not for operators reading
    /// the `stages_ms` log line.
    const fn label(self) -> &'static str {
        match self {
            Self::Route => "Working out what you're asking",
            Self::History => "Loading the conversation",
            Self::RewriteExpand => "Rewriting the question",
            Self::Embed => "Embedding the question",
            Self::SearchVector => "Searching records by meaning",
            Self::SearchText => "Searching records by keyword",
            Self::Rrf => "Merging both result sets",
            Self::Rerank => "Re-ranking the best matches",
            Self::Gate => "Checking the matches are good enough",
            Self::Prompt => "Assembling the evidence",
            Self::Ttft => "Waiting for the model",
            Self::Generate => "Writing the answer",
            Self::Persist => "Saving the conversation",
            Self::Plan => "Planning the query",
            Self::Validate => "Checking the query is safe",
            Self::Execute => "Querying the database",
            Self::Narrate => "Writing the answer",
        }
    }

    /// `Ttft` is a derived measurement, not a step the user is waiting through,
    /// so it never reaches the strip. Everything else does.
    const fn user_visible(self) -> bool {
        !matches!(self, Self::Ttft)
    }
}

#[derive(Debug, Default)]
struct Summary {
    route: Option<String>,
    router_tier: Option<u8>,
    router_cached: Option<bool>,
    candidates_in: Option<usize>,
    candidates_out: Option<usize>,
    rerank_top_score: Option<f64>,
    gated: Option<bool>,
    token_events: u64,
    output_chars: u64,
    ttft_ms: Option<u64>,
    stages_ms: BTreeMap<&'static str, u64>,
}

const MAX_METRIC_SAMPLES: usize = 2_048;

#[derive(Debug, Default)]
struct Metrics {
    requests: BTreeMap<String, u64>,
    outcomes: BTreeMap<String, u64>,
    total_ms: VecDeque<u64>,
    ttft_ms: VecDeque<u64>,
    stage_total_ms: BTreeMap<&'static str, u64>,
    stage_count: BTreeMap<&'static str, u64>,
}

#[derive(Debug, Serialize)]
pub struct MetricsSummary {
    pub requests: BTreeMap<String, u64>,
    pub outcomes: BTreeMap<String, u64>,
    pub total_latency_ms: Percentiles,
    pub ttft_ms: Percentiles,
    pub stage_average_ms: BTreeMap<&'static str, u64>,
}

#[derive(Debug, Serialize)]
pub struct Percentiles {
    pub samples: usize,
    pub p50: Option<u64>,
    pub p95: Option<u64>,
    pub p99: Option<u64>,
}

static METRICS: LazyLock<Mutex<Metrics>> = LazyLock::new(|| Mutex::new(Metrics::default()));

/// Where live stage transitions are published, once a request has been given a
/// client run id. `None` for requests the client isn't watching (the eval
/// harness, `/search`, any caller that omits `run_id`).
#[derive(Debug)]
struct ProgressSink {
    hub: crate::progress::RunProgressHub,
    run_id: String,
}

#[derive(Debug)]
struct Inner {
    request_id: String,
    endpoint: &'static str,
    started: Instant,
    summary: Mutex<Summary>,
    emitted: AtomicBool,
    /// Set at most once, before any stage runs. Behind a lock rather than a
    /// `OnceLock` only because the run id arrives from the request body, after
    /// `RequestTrace::new`.
    progress: Mutex<Option<ProgressSink>>,
}

impl Inner {
    fn summary(&self) -> MutexGuard<'_, Summary> {
        self.summary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, outcome: &'static str, error_class: Option<&'static str>) {
        if self.emitted.swap(true, Ordering::AcqRel) {
            return;
        }

        let summary = self.summary();
        let total_ms = duration_ms(self.started.elapsed());
        record_metrics(self.endpoint, outcome, total_ms, &summary);
        let stages_json = serde_json::to_string(&summary.stages_ms).unwrap_or_else(|_| "{}".into());
        tracing::info!(
            event = "request_summary",
            request_id = %self.request_id,
            endpoint = self.endpoint,
            route = summary.route.as_deref(),
            router_tier = summary.router_tier,
            router_cached = summary.router_cached,
            candidates_in = summary.candidates_in,
            candidates_out = summary.candidates_out,
            rerank_top_score = summary.rerank_top_score,
            gated = summary.gated,
            token_events = summary.token_events,
            output_chars = summary.output_chars,
            ttft_ms = summary.ttft_ms,
            total_ms,
            outcome,
            error_class,
            stages_ms = %stages_json,
            "request summary"
        );
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if !self.emitted.load(Ordering::Acquire) {
            self.emit("cancelled_or_setup_error", Some("request_incomplete"));
        }
        // A request that never reached its stream (auth failure, cancelled client)
        // still has a progress subscriber waiting on it. Close it now rather than
        // leaving the strip spinning until the stream's idle timeout.
        if let Ok(mut slot) = self.progress.lock()
            && let Some(sink) = slot.take()
        {
            sink.hub.publish(
                &sink.run_id,
                crate::progress::ProgressEvent::new("done", "", "done"),
            );
            sink.hub.remove(&sink.run_id);
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestTrace {
    inner: Arc<Inner>,
}

impl RequestTrace {
    pub fn new(endpoint: &'static str) -> Self {
        Self {
            inner: Arc::new(Inner {
                request_id: uuid::Uuid::now_v7().to_string(),
                endpoint,
                started: Instant::now(),
                summary: Mutex::new(Summary::default()),
                emitted: AtomicBool::new(false),
                progress: Mutex::new(None),
            }),
        }
    }

    /// Start publishing live stage transitions for `run_id`. Call once, before
    /// the first stage; requests without a client run id simply never call it and
    /// pay nothing.
    pub fn attach_progress(&self, hub: crate::progress::RunProgressHub, run_id: String) {
        if let Ok(mut slot) = self.inner.progress.lock() {
            *slot = Some(ProgressSink { hub, run_id });
        }
    }

    fn publish(&self, event: crate::progress::ProgressEvent) {
        let Ok(slot) = self.inner.progress.lock() else {
            return;
        };
        if let Some(sink) = slot.as_ref() {
            sink.hub.publish(&sink.run_id, event);
        }
    }

    fn stage_started(&self, stage: Stage) {
        if stage.user_visible() {
            self.publish(crate::progress::ProgressEvent::new(
                stage.name(),
                stage.label(),
                "start",
            ));
        }
    }

    fn stage_ended(&self, stage: Stage, elapsed: Duration) {
        if stage.user_visible() {
            self.publish(
                crate::progress::ProgressEvent::new(stage.name(), stage.label(), "end")
                    .with_ms(duration_ms(elapsed)),
            );
        }
    }

    /// Attach a PHI-free detail to a stage already reported — row counts, kept
    /// passages, the backend that answered. Shown as the strip's sub-label.
    pub fn stage_detail(&self, stage: Stage, detail: impl Into<String>) {
        self.publish(
            crate::progress::ProgressEvent::new(stage.name(), stage.label(), "end")
                .with_detail(detail),
        );
    }

    /// Close the run's progress stream and drop its buffer. Always call this when
    /// the answer stream ends, however it ended.
    pub fn progress_finished(&self) {
        self.publish(crate::progress::ProgressEvent::new("done", "", "done"));
        if let Ok(mut slot) = self.inner.progress.lock()
            && let Some(sink) = slot.take()
        {
            sink.hub.remove(&sink.run_id);
        }
    }

    pub fn request_id(&self) -> &str {
        &self.inner.request_id
    }

    pub async fn time<T>(&self, stage: Stage, future: impl Future<Output = T>) -> T {
        let started = Instant::now();
        self.stage_started(stage);
        let request_id = self.request_id().to_string();
        let result = match stage {
            Stage::Route => {
                future
                    .instrument(tracing::info_span!("route", %request_id))
                    .await
            }
            Stage::History => {
                future
                    .instrument(tracing::info_span!("history", %request_id))
                    .await
            }
            Stage::RewriteExpand => {
                future
                    .instrument(tracing::info_span!("rewrite_expand", %request_id))
                    .await
            }
            Stage::Embed => {
                future
                    .instrument(tracing::info_span!("embed", %request_id))
                    .await
            }
            Stage::SearchVector => {
                future
                    .instrument(tracing::info_span!("search_vector", %request_id))
                    .await
            }
            Stage::SearchText => {
                future
                    .instrument(tracing::info_span!("search_text", %request_id))
                    .await
            }
            Stage::Rerank => {
                future
                    .instrument(tracing::info_span!("rerank", %request_id))
                    .await
            }
            Stage::Generate => {
                future
                    .instrument(tracing::info_span!("generate", %request_id))
                    .await
            }
            Stage::Persist => {
                future
                    .instrument(tracing::info_span!("persist", %request_id))
                    .await
            }
            Stage::Plan => {
                future
                    .instrument(tracing::info_span!("plan", %request_id))
                    .await
            }
            Stage::Execute => {
                future
                    .instrument(tracing::info_span!("execute", %request_id))
                    .await
            }
            Stage::Narrate => {
                future
                    .instrument(tracing::info_span!("narrate", %request_id))
                    .await
            }
            Stage::Rrf | Stage::Gate | Stage::Prompt | Stage::Ttft | Stage::Validate => {
                future.await
            }
        };
        let elapsed = started.elapsed();
        self.record_duration(stage, elapsed);
        self.stage_ended(stage, elapsed);
        result
    }

    pub fn time_sync<T>(&self, stage: Stage, operation: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        self.stage_started(stage);
        let request_id = self.request_id();
        let result = match stage {
            Stage::Rrf => tracing::info_span!("rrf", %request_id).in_scope(operation),
            Stage::Gate => tracing::info_span!("gate", %request_id).in_scope(operation),
            Stage::Prompt => tracing::info_span!("prompt", %request_id).in_scope(operation),
            Stage::Validate => tracing::info_span!("validate", %request_id).in_scope(operation),
            _ => operation(),
        };
        let elapsed = started.elapsed();
        self.record_duration(stage, elapsed);
        self.stage_ended(stage, elapsed);
        result
    }

    pub fn stage_guard(&self, stage: Stage) -> StageGuard {
        self.stage_started(stage);
        StageGuard {
            trace: self.clone(),
            stage,
            started: Instant::now(),
        }
    }

    pub fn set_route(&self, route: impl Into<String>, tier: Option<u8>, cached: Option<bool>) {
        let mut summary = self.inner.summary();
        summary.route = Some(route.into());
        summary.router_tier = tier;
        summary.router_cached = cached;
    }

    pub fn set_retrieval(
        &self,
        candidates_in: usize,
        candidates_out: usize,
        top_score: Option<f64>,
    ) {
        {
            let mut summary = self.inner.summary();
            summary.candidates_in = Some(candidates_in);
            summary.candidates_out = Some(candidates_out);
            summary.rerank_top_score = top_score;
        }
        self.stage_detail(
            Stage::Rerank,
            format!("kept the best {candidates_out} of {candidates_in} passages"),
        );
    }

    pub fn set_gated(&self, gated: bool) {
        self.inner.summary().gated = Some(gated);
    }

    pub fn record_output(&self, text: &str) {
        let mut summary = self.inner.summary();
        if summary.ttft_ms.is_none() {
            let elapsed = duration_ms(self.inner.started.elapsed());
            let request_id = self.request_id();
            tracing::info_span!("ttft", %request_id, elapsed_ms = elapsed).in_scope(|| {});
            summary.ttft_ms = Some(elapsed);
            summary.stages_ms.insert(Stage::Ttft.name(), elapsed);
        }
        summary.token_events += 1;
        summary.output_chars += text.chars().count() as u64;
    }

    pub fn finish(&self, outcome: &'static str, error_class: Option<&'static str>) {
        self.inner.emit(outcome, error_class);
    }

    fn record_duration(&self, stage: Stage, duration: Duration) {
        let elapsed = duration_ms(duration);
        let mut summary = self.inner.summary();
        *summary.stages_ms.entry(stage.name()).or_default() += elapsed;
    }
}

fn record_metrics(endpoint: &str, outcome: &str, total_ms: u64, summary: &Summary) {
    let mut metrics = METRICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *metrics.requests.entry(endpoint.to_string()).or_default() += 1;
    *metrics.outcomes.entry(outcome.to_string()).or_default() += 1;
    push_sample(&mut metrics.total_ms, total_ms);
    if let Some(ttft_ms) = summary.ttft_ms {
        push_sample(&mut metrics.ttft_ms, ttft_ms);
    }
    for (&stage, &elapsed) in &summary.stages_ms {
        *metrics.stage_total_ms.entry(stage).or_default() += elapsed;
        *metrics.stage_count.entry(stage).or_default() += 1;
    }
}

fn push_sample(samples: &mut VecDeque<u64>, sample: u64) {
    if samples.len() == MAX_METRIC_SAMPLES {
        samples.pop_front();
    }
    samples.push_back(sample);
}

fn percentiles(samples: &VecDeque<u64>) -> Percentiles {
    let mut values: Vec<u64> = samples.iter().copied().collect();
    values.sort_unstable();
    let pick = |percent: usize| {
        (!values.is_empty()).then(|| {
            let index = ((values.len() - 1) * percent).div_ceil(100);
            values[index]
        })
    };
    Percentiles {
        samples: values.len(),
        p50: pick(50),
        p95: pick(95),
        p99: pick(99),
    }
}

pub fn metrics_summary() -> MetricsSummary {
    let metrics = METRICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stage_average_ms = metrics
        .stage_total_ms
        .iter()
        .map(|(&stage, &total)| {
            let count = metrics.stage_count.get(stage).copied().unwrap_or(1);
            (stage, total / count)
        })
        .collect();
    MetricsSummary {
        requests: metrics.requests.clone(),
        outcomes: metrics.outcomes.clone(),
        total_latency_ms: percentiles(&metrics.total_ms),
        ttft_ms: percentiles(&metrics.ttft_ms),
        stage_average_ms,
    }
}

pub struct StageGuard {
    trace: RequestTrace,
    stage: Stage,
    started: Instant,
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        self.trace.record_duration(self.stage, elapsed);
        self.trace.stage_ended(self.stage, elapsed);
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_stages_and_output_without_payload_text() {
        let trace = RequestTrace::new("test");
        trace.set_route("semantic", Some(2), Some(true));
        trace.time_sync(Stage::Prompt, || 42);
        trace.set_retrieval(30, 6, Some(0.91));
        trace.set_gated(false);
        trace.record_output("sensitive output is counted, not retained");

        let summary = trace.inner.summary();
        assert_eq!(summary.route.as_deref(), Some("semantic"));
        assert_eq!(summary.candidates_in, Some(30));
        assert_eq!(summary.candidates_out, Some(6));
        assert_eq!(summary.token_events, 1);
        assert_eq!(summary.output_chars, 41);
        assert!(summary.stages_ms.contains_key("prompt"));
    }

    #[test]
    fn finish_is_idempotent() {
        let trace = RequestTrace::new("test");
        trace.finish("ok", None);
        trace.finish("stream_error", Some("generation"));
        assert!(trace.inner.emitted.load(Ordering::Acquire));
    }
}
