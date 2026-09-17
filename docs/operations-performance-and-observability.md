# Operations, Performance, and Observability

**Authoritative status:** code-verified on 2026-09-02. Targets are not reported as achieved unless a committed result proves them.

## Current operating model

The system is a single on-premises Rocket process backed by DocumentDB, local SQL sources, Foundry Local, and process-global fastembed models. Concurrency is deliberately bounded before attempting horizontal scale.

### Implemented controls

| Area | Current implementation |
| --- | --- |
| Request admission | Configurable semaphores cap generations, retrieval requests, global ingestions, and ingestions per source. Acquisition has a configured timeout and returns `429` on saturation. |
| Readiness | `GET /ready` checks DocumentDB, required indexes, Foundry availability, warmup state, and free admission capacity. `GET /health` remains liveness-oriented and reports DocumentDB degradation without failing the process. |
| Retrieval concurrency | Query embeddings are batched; vector and text searches for all query variants run concurrently; results are fused with RRF. |
| Caches | A 1,024-entry normalized query-embedding LRU and a configurable Tier-2 router LRU are process-local. Hardware detection and model state also cache appropriate static data. Final PHI-bearing answers are not cached. |
| Warmup | Background startup warmup initializes embedder/reranker and, when configured, registers execution providers and warms the SQL model. It does not block Rocket startup; readiness reflects the outcome. |
| Adaptive work | Short, quoted, and identifier-like queries can skip model expansion. Retrieval chooses a rerank depth between at least `max(top_k, 8)` and the configured maximum using an RRF score-drop heuristic, then trims very weak tail passages. |
| Ingestion safety | Source rows are fetched in bounded pages, checkpoints are persisted, inactive generation records are written, and a transaction switches the active generation. Failed refreshes retain the prior active generation. Abandoned running jobs are recovered at startup and can be resumed. |
| Conversation bounds | Working memory uses a token-bounded recent tail plus write-behind rolling summaries. Stored messages have a byte cap, and optional conversation retention runs at boot and daily. |
| Client streaming | Chat, agent, model, ingestion, and log streams are routed through the bridge; UI token updates are batched. Concurrent conversations are keyed by run id, and the bridge supports per-run and logout-wide aborts. |
| CI | Windows CI checks and tests the server, installs/builds/type-checks the frontend, and validates deterministic eval fixtures. |

## Observability

### Request summaries and metrics

`RequestTrace` records PHI-safe stage durations and emits one `request_summary` trace event when a request completes, errors during streaming, or is dropped incomplete. Current fields include endpoint, request id, route/tier/cache status, candidate counts, rerank top score, gate result, token-event/output-character counts, TTFT, total time, outcome, error class, and a stage-duration map. Payload text, prompts, JWTs, and source credentials are not intentionally retained by this instrumentation.

The administrator-only `GET /metrics/summary` endpoint exposes process-lifetime request/outcome counts, p50/p95/p99 total latency and TTFT over a bounded 2,048-sample window, and average stage durations. Metrics reset on restart and are not Prometheus-compatible or persisted.

**Partial instrumentation:** stable timing exists across the principal chat/search/agent, retrieval, and structured stages, but the summary is not a complete queue-depth, memory, device-utilization, ingestion-throughput, or per-cache telemetry system. Admission exposes available capacity in readiness, not historical waits/rejections. Validate endpoint coverage whenever adding a new request path.

**Planned metrics** (see [plans/new/02 §9](../new/02-intent-router-v3.md) and [plans/new/08 §3](../new/08-evaluation-and-rollout.md)): a **Tier-2 invocation rate** (share of routed requests that reached the model classifier; target ≤ 25 % on the eval fixture), per-rung latency percentiles derived from `provenance.elapsed_ms`, an `ir_shadow` comparison counter while `ONPREM_SQL_IR_ENABLED=false`, and a v2-vs-v3 shadow route disagreement count while `ONPREM_ROUTER_V3=false`. These are reported by the eval runner first; exposing them in `GET /metrics/summary` is part of the same workstream.

### Run progress stream

`/chat` and `/agents` complete routing, retrieval, planning, and source queries before their answer stream opens, so pipeline stages cannot be interleaved into the answer SSE itself. Clients that want live progress mint a `run_id`, subscribe to `GET /runs/<run_id>/progress`, and post the same id with the question. `RequestTrace` then publishes a start and an end for every stage it times — stage key, human-readable label, duration, and an optional PHI-free detail such as kept-passage or row counts.

The hub is process-local and best-effort: it buffers up to 64 events per run so a subscriber that connects after the POST replays what it missed, drops abandoned runs after 10 minutes, terminates a stream on the run's `done` marker or 180 s of silence, and never fails a request when publishing fails. The Tauri bridge opens one such subscription per run alongside the answer stream and relays events as `chat://stage` / `agent://stage`; the React activity strips render them in place of a single indeterminate spinner. Omitting `run_id` disables the fan-out entirely (the eval harness and `/search` do).

### Live trace stream

The tracing subscriber sends the same filtered events to the terminal and an in-process `LogHub`. The hub retains 300 recent lines and broadcasts through a 512-slot channel. Authenticated clients use `GET /logs/stream`; late joiners receive the replay ring, and lagged clients receive a dropped-line warning. The Tauri bridge maintains one relay per session, and the React store keeps a bounded 1,000-line view filtered by module category.

This is operational visibility, not durable logging:

- no file sink, rotation, export, restart persistence, or retention policy exists;
- the replay ring is process-local;
- byte-level third-party download progress is unavailable unless the SDK emits it; and
- log content still requires ongoing PHI review even though request summaries are designed to be PHI-free.

## Performance implementation status

### Implemented

- Rewrite plus expansion is combined into one model call when history and expansion are both needed; no-history requests avoid rewrite, and cheap exact/short queries skip expansion.
- Multi-query embeddings are issued as one fastembed batch, with cache hits removed before inference.
- Vector and lexical searches execute concurrently across query variants.
- Hybrid results use rank-based RRF, row-level deduplication, adaptive rerank candidate depth, score gating, context budgets, and weak-tail trimming.
- Embedder and reranker synchronous APIs run in `spawn_blocking`, protecting Tokio's reactor.
- Ingestion is paged and resumable, uses configurable embedding batches, persists checkpoints, and performs generation-based cutover.
- Ingestion progress combines process-local broadcast notifications with durable job snapshots.
- Request summaries, bounded metrics, readiness, admission controls, load tooling, deterministic eval fixtures, and CI exist.
- Conversation memory compaction and optional retention exist.

### Partial or constrained

- **Source paging uses `OFFSET`.** The connector API accepts an introspected primary-key sort, but all current pages advance by numeric offset; this is bounded in memory but can become increasingly expensive and can be unstable under concurrent source changes. Keyset/cursor paging is not implemented.
- **Inference remains mutex-serialized.** The process-global embedder and reranker each use a single `Mutex`. Admission limits requests around them, but there are no multiple model workers or measured micro-batches across requests.
- **Early SSE is partial.** Routing/retrieval/model setup is still predominantly completed before `/chat` returns its event stream, so routed/citation/token events all arrive once it opens. Pre-TTFT progression is now observable, but out of band: stages are published to the separate run progress stream described above rather than emitted early on the answer stream.
- **Cancellation is client-transport-level.** The bridge can abort its HTTP stream and releases local run tracking. There is no proven end-to-end cancellation token propagated through already-running retrieval, blocking fastembed calls, source queries, and Foundry generation.
- **IVF is fixed.** The vector index is `vector-ivf` with `numLists=100`; the implementation does not resize it from corpus cardinality or expose index metadata.
- **Generation cleanup is eager, not grace-period background cleanup.** After cutover, old inactive generations are deleted immediately on a best-effort basis.
- **Progress persistence remains write-heavy.** A bounded log of up to 500 entries is serialized into each job snapshot after a page rather than stored as deltas.
- **List scaling is incomplete.** Audit and explorer views are paged, and working-memory reads are bounded, but conversation lists and full message-history responses remain unpaged. Dashboard counts still require review at production corpus scale.

### Planned and unvalidated

- Representative corpus and concurrency capacity profiles.
- Eight-hour 10/25/50-client soak results and bounded-RSS evidence.
- HNSW support/benchmarking, dynamic IVF sizing, and recall-versus-latency reports.
- Full cancellation propagation and explicit stage deadlines beyond admission and selected model/SQL operations.
- Durable local log rotation, redaction audits, export, and retention.
- Queue-depth/wait/rejection metrics, host RSS/task growth metrics, and GPU/NPU utilization capture.
- Materialized dashboard summaries and cursor paging for every growing list.
- Ingestion stage overlap via bounded channels and demonstrated throughput gains.
- Client long-conversation render profiling, virtualization, and bundle budgets.
- Horizontal scaling. Login throttling, caches, metrics, and stream hubs are process-local and would require shared coordination or aggregation.

### Planned latency budgets (router v3 + StructuredExecutor)

**Status: Planned — see [plans/new/08-evaluation-and-rollout.md §3](../new/08-evaluation-and-rollout.md).** Gate values on the dev host, not achieved results:

| Measure | Budget |
| --- | --- |
| Deterministic structured turn (Tier 1.5 parse → compile → execute), p50 | < 300 ms |
| Routing latency when Tier 2 is not invoked, median | < 50 ms |
| Model-planned structured turn, p50 | < 8 s |
| Tier-2 model classifier invocation rate on the fixture | ≤ 25 % |
| Capability answer ("what can you do?") | < 100 ms, no model call |
| Follow-up resolved via focus (e.g. A10b) | < 400 ms |
| Binding rebuild for 64 tables | < 5 s |

Deterministic-first routing exists chiefly to avoid the Tier-2 model call (2–4 s) and Foundry LRU unload/reload (40 s+); the Tier-2 rate must therefore be measured in every eval report.

## Operating configuration

Important knobs include:

- `ONPREM_MAX_ACTIVE_GENERATIONS`, `ONPREM_MAX_ACTIVE_RETRIEVALS`, `ONPREM_MAX_ACTIVE_INGESTIONS`, `ONPREM_MAX_INGESTIONS_PER_SOURCE`, `ONPREM_ADMISSION_TIMEOUT_MS`;
- `ONPREM_WARMUP_ENABLED`, `ONPREM_ROUTER_CACHE_SIZE`;
- `ONPREM_MULTI_QUERY_ENABLED`, `ONPREM_MULTI_QUERY_COUNT`, `ONPREM_RETRIEVE_PER_SIDE`, `ONPREM_RERANK_TOP_N`, `ONPREM_CONTEXT_TOP_K`, and `ONPREM_SCORE_GATE`;
- `ONPREM_INGEST_PAGE_SIZE` and `ONPREM_INGEST_EMBED_BATCH_SIZE`; and
- `RUST_LOG`, which controls both terminal and in-app trace visibility.

### Planned configuration keys

**Status: Planned — not read by `config.rs` today.** Introduced by [plans/new/01](../new/01-service-line-ontology-and-schema-binding.md)–[08](../new/08-evaluation-and-rollout.md); defaults in parentheses.

| Key | Plan | Purpose |
| --- | --- | --- |
| `ONPREM_BINDING_ENABLED` (true) | 08 | Build and load per-source schema bindings |
| `ONPREM_BINDING_MIN_CONFIDENCE` (0.55) | 01 | Concept confidence below which a table is not bound / a line is not usable |
| `ONPREM_BINDING_ENUM_MAX` (25) | 01 | Max distinct values collected into `enum_values` |
| `ONPREM_BINDING_MAX_HOPS` (3) | 01 | BFS limit for `patient_path` |
| `ONPREM_ROUTER_V3` (false at first) | 08 | Shadow-run router v3 beside v2; flip when accuracy ≥ 0.95 |
| `ONPREM_ROUTER_DETERMINISTIC_FIRST` (true) | 02 | Run the `QuerySpec` parse before the Tier-2 model classifier |
| `ONPREM_ROUTER_CLARIFY_ENABLED` (true) | 02 | Allow the bounded clarify branch |
| `ONPREM_ROUTER_MODEL_MIN_CONFIDENCE` (0.5) | 02 | Below this, a short question with no focus clarifies; otherwise fail open to semantic |
| `ONPREM_SQL_IR_ENABLED` (false at first) | 08 | Shadow-run the IR compiler beside the legacy templates |
| `ONPREM_EXEC_TOTAL_TIMEOUT_SECS` (120) | 04 | Total executor budget; exhaustion skips to filtered retrieval |
| `ONPREM_HYBRID_COHORT_MAX` (200) | 04 | Cap on cohort row keys passed into the retrieval filter |
| `ONPREM_RETRIEVAL_FILTER_RELAX` (true) | 04 | Retry once unfiltered when an inferred filter empties results |
| `ONPREM_FOCUS_ENABLED` (true) | 06 | Maintain and consume `ConversationFocus` |
| `ONPREM_FOCUS_PATIENT_TTL_TURNS` (0 = keep) | 06 | Turns before a focus patient expires |
| `ONPREM_FOCUS_TOP_LABELS` (5) | 06 | Labels kept in the last-result digest |
| `ONPREM_SUGGESTIONS_MAX` (4) | 06 | Suggestions emitted per answer |
| `ONPREM_MODEL_PLAN_SPEC` (SQL model) | 05 | Model for the `PlanSpec` role |

Existing `ONPREM_NL2SQL_PLAN_TIMEOUT_SECS` / `ONPREM_NL2SQL_TIMEOUT_SECS` become the executor's per-rung `sql_plan` / `sql_exec` deadlines.

Do not tune these from estimates alone. Record hardware, execution provider, model variants, corpus size, request mix, concurrency, configuration, latency percentiles, errors, quality metrics, and peak memory for each comparison.

## Validation workflow

1. Run deterministic fixture validation in CI.
2. On an isolated synthetic-data host, run the local eval suites and save their JSON output.
3. Capture `GET /metrics/summary`, readiness, configuration, model/EP details, corpus size, and host profile.
4. Use [`perf/load.mjs`](../../perf/load.mjs) for a basic concurrent `/chat` ladder. It reports status counts and p50/p95/p99 end-to-end response latency; it does not measure RSS, queue depth, device utilization, or recovery.
5. Perform ingestion interruption, dependency failure, and restart trials using the production runbook.
6. Do not claim a capacity or SLO result until the measurements are committed with the tested revision.

The reference TTFT and soak values in the plans are acceptance targets, not current guarantees.

## Source plans consolidated

- [04 — Live Log Windows](../old/04-log-streaming.md) and [older log-streaming guide](../old/docs/log-streaming.md): implemented trace broadcast and UI relay, corrected here to emphasize ephemeral retention.
- [20 — Performance Fast Path](../old/20-performance-fast-path.md): combined rewrite/expansion, concurrent retrieval, caches, warmup, and now-implemented adaptive rerank; its skipped/deferred labels are superseded by code where noted.
- [21 — Evaluation Harness](../old/21-eval-harness.md): instrumentation and deterministic portions are now implemented; judged evaluation remains open.
- [23 — Performance/Efficiency Roadmap](../old/23-performance-efficiency-roadmap.md): consolidated status for admission, paging, generations, memory, early SSE, IVF/HNSW, and scaling.
- [24 — Production Validation Runbook](../old/24-production-validation-runbook.md): procedure exists; no committed capacity/soak result is inferred from it.
- [Sol Findings](../old/Sol-Findings.md) and [Sol implementation plan](../old/Sol-implementation%20plan.md): duplicate recommendations consolidated and stale “no CI/no paging/no admission” claims corrected from current code.
