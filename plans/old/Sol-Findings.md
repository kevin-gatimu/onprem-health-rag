
# On-Prem Health RAG — Performance and Efficiency Roadmap

## Executive recommendation

Do not optimize individual algorithms blindly. First add repeatable evaluation and per-stage timing, then address the three demonstrated bottlenecks: unbounded whole-table ingestion, globally serialized inference, and pre-stream chat work. Preserve the existing privacy boundary and accuracy stack while introducing bounded concurrency and adaptive work.

## Current strengths

- Route-level lazy loading in the React client.
- `requestAnimationFrame` batching for chat/agent tokens and log events.
- Batched query embeddings and concurrent vector/text retrieval fan-out.
- Query embedding and routing caches.
- Warmup support for embedder and reranker.
- Hybrid retrieval, RRF, cross-encoder reranking, score gating, citations, and structured routing.
- Database indexes for conversation and message access patterns.

## P0 — Measure and stabilize

### 1. Implement the evaluation and instrumentation harness

The repository has no CI workflows, no frontend tests, and plan 21 remains planned. Add stable timing spans for route, rewrite/expand, embed, vector search, text search, RRF, rerank, gate, prompt, TTFT, generation, structured execution, and NL-to-SQL. Add a deterministic judge-free smoke suite using the seeded development data.

Acceptance targets:

- Every chat/agent request emits one structured summary record.
- Retrieval smoke suite runs in under 2 minutes.
- Track p50/p95/p99 TTFT and total latency by route.
- Track hit-rate@6, MRR, refusal precision/recall, route accuracy, exact structured-result match, and error rate.

### 2. Stream ingestion instead of buffering tables

Current ingestion fetches an entire table, creates all chunks in memory, then embeds and inserts sequentially. It also deletes existing indexed data before the replacement is known to be valid.

Change to keyset/cursor pagination, bounded channels, and a pipeline:

`source page -> chunk -> embed batch -> bulk upsert -> checkpoint`

Write into a versioned/staging generation and atomically switch the active generation after success. Resume from checkpoints after failure.

Acceptance targets:

- Peak RSS remains bounded as source size grows.
- At least 1.5x ingestion throughput over the current baseline.
- Failed re-ingestion leaves the previous generation searchable.
- Job progress writes contain deltas, not the full 500-entry log on every batch.

### 3. Add global backpressure and timeouts

A request can issue several searches concurrently, but there is no visible global search semaphore. The embedder and reranker are each protected by one process-global mutex, serializing concurrent inference.

Add bounded queues/semaphores for retrieval, embedding, reranking, generation, and ingestion. Add per-stage deadlines and cancellation propagation. Start with one inference worker per model/device, then benchmark controlled multi-instance or micro-batching where memory allows.

Acceptance targets:

- Stable memory and latency under 10–50 concurrent clients.
- Overload returns a clear 429/503 rather than causing unbounded queue growth.
- Cancellation releases permits and stops downstream work.

## P1 — Reduce latency without reducing quality

### 4. Make query expansion adaptive

The no-history path still calls the model for expansion whenever expansion is enabled. Skip expansion for short exact-token queries, conversational turns, strong lexical/code queries, and cached/repeated requests. Escalate to multi-query only when the route or first-pass confidence warrants it.

### 5. Tune reranking with measured budgets

Reranking is globally serialized and defaults to 30 candidates. Use an RRF score-elbow or first-pass confidence rule to select 8–30 candidates dynamically. Expose batch size and a fast/accurate preset. Do not change the default model until the eval suite proves non-inferiority.

### 6. Open SSE earlier

The chat route performs routing, history loading, retrieval, reranking, and stream creation before returning the SSE response. Open the stream early and emit `routing`, `rewriting`, `retrieving`, `reranking`, and `generating` status events. This improves perceived latency and enables cancellation.

Targets on a warm system:

- Conversational TTFT: p95 under 1 second.
- Semantic no-history TTFT: p95 under 1.5 seconds.
- Semantic with-history TTFT: p95 under 2.5 seconds.
- Structured rows event: p95 under 2.5 seconds.

### 7. Calibrate the safety gate

Applying sigmoid to reranker logits bounds scores but does not calibrate them. Measure score distributions for relevant, irrelevant, and no-answer queries; select thresholds against a health-record-specific validation set. Report refusal precision and recall. Consider thresholds per reranker/model version.

### 8. Add metadata prefilters and parent-row expansion

Apply source/table/patient/time/code constraints before ANN where supported. Retrieve small chunks but expand selected hits to row/parent context after reranking. This improves precision, reduces candidate waste, and avoids presenting fragments without clinical context.

## P1 — Storage and API efficiency

### 9. Size the vector index from corpus scale

The vector index has a fixed IVF `numLists=100`. Benchmark IVF settings against corpus sizes and validate HNSW support in the deployed DocumentDB version. Store index version/config and expose recall/latency measurements before switching defaults.

### 10. Bound list APIs and dashboard scans

Conversation and message endpoints collect all matching documents. Add cursor pagination. The dashboard recomputes distinct row counts through collection-wide grouping; maintain ingest-time counters or a materialized summary instead.

### 11. Finish memory compaction

The generation/rewrite path uses a fixed recent-message tail, while the UI loads complete conversations. Add rolling summaries, a verbatim recent tail, token budgets, pagination, retention limits, and redaction-aware deletion.

## P2 — Client responsiveness

- Keep existing route lazy loading and rAF token batching.
- Throttle auto-scroll to animation frames and only force bottom-scroll when the user is already near the bottom.
- Memoize expensive Markdown/chart rendering and avoid reparsing the entire growing streamed answer every frame; render plain streaming text or parse at a lower cadence, then render full Markdown at completion.
- Paginate or virtualize long conversation lists, message histories, audit tables, and large data grids.
- Add bundle-size budgets and React render profiling to CI.
- Retry the always-on log stream with capped exponential backoff; suspend it when not needed if operational logs are high volume.

## Production roadmap

### Phase 1: Baseline (1–2 weeks)

- Per-stage tracing and metrics.
- Judge-free smoke evaluation.
- Load-test scripts for chat, retrieval, ingestion, and reconnect behavior.
- CI for Rust build/test, TypeScript check/build, and smoke tests.

### Phase 2: Bounded execution (2–3 weeks)

- Global semaphores, deadlines, cancellation, and overload behavior.
- Streaming ingestion with checkpoints and generation-based cutover.
- Cursor pagination and materialized dashboard counters.

### Phase 3: Adaptive RAG (2–3 weeks)

- Adaptive query expansion and rerank depth.
- Calibrated refusal thresholds.
- Metadata prefilters and parent-row context.
- Early SSE status events.

### Phase 4: Scale validation (1–2 weeks)

- IVF/HNSW corpus-size benchmark.
- 10/25/50-client soak tests.
- Failure injection: DocumentDB restart, source timeout, model crash/OOM, dropped SSE client, and partial ingestion.
- Publish hardware-specific capacity profiles.

## Release gates

- No regression in hit-rate@6, MRR, refusal precision, citation correctness, or exact structured results.
- p95 targets met on declared reference hardware.
- No unbounded memory growth during a multi-million-row ingest or 8-hour soak.
- Previous index remains available after ingestion failure.
- All model calls and evaluation remain local/on-premises.
