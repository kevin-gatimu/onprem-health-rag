
# 23 — Performance, Efficiency, and Production-Scale Roadmap

> **Status: PLANNED.** This is the execution plan that ties together the unfinished work in
> [19](19-retrieval-and-faithfulness.md), [20](20-performance-fast-path.md),
> [21](21-eval-harness.md), and [22](22-chat-memory-and-compaction.md). The order is deliberate:
> measure first, bound resource use second, then tune quality and latency from evidence. Every model
> call remains local; PHI must never leave the premises.

## Goals

1. Keep latency predictable as concurrent users and corpus size grow.
2. Bound memory and queue growth during chat, retrieval, and ingestion.
3. Improve time-to-first-token (TTFT) without weakening grounding or citations.
4. Make ingestion resumable and prevent failed refreshes from removing searchable data.
5. Establish repeatable quality and performance gates before changing models or retrieval settings.
6. Keep the React/Tauri client responsive during long streams and large histories.

## Non-goals

- No cloud inference, telemetry, evaluation judge, or hosted observability service.
- No speculative model replacement without local evaluation evidence.
- No horizontal server cluster in the first implementation; make one server predictable first.
- No general-purpose distributed job platform. A persisted ingestion state machine is sufficient.
- No automatic index migration during peak use; index changes remain explicit admin operations.

## Baseline findings (2026-08-31)

### Already good — preserve

- Query embeddings are batched and cached in `onprem-rag-server/src/embed/mod.rs`.
- Vector and text retrieval fan-out runs concurrently in `retrieval/mod.rs`.
- The app lazy-loads feature routes in `onprem-rag-app/src/app/routes.tsx`.
- Chat, agent, and log tokens are batched onto animation frames in
  `onprem-rag-app/src/lib/bridgeEvents.ts`.
- Conversation/message access patterns have supporting indexes in `documentdb/mod.rs`.
- Warmup exists for fastembed models, and conversational routing can bypass retrieval.

### Demonstrated bottlenecks

1. **No objective baseline.** Plan 21 remains unimplemented; there is no repository CI workflow or
   frontend test suite. Latency claims cannot yet be validated or protected from regression.
2. **Whole-table ingestion buffering.** `ingest/mod.rs` calls `fetch_table`, stores all rows, then
   builds all chunks before processing batches. Peak memory therefore scales with table size.
3. **Unsafe refresh cutover.** Existing table records are deleted before the replacement batches
   finish. A failed ingest can leave that table partially indexed or empty.
4. **Sequential ingest stages.** Each batch is embedded and only then inserted; CPU/model and DB I/O
   do not overlap. Every batch also rewrites the complete capped job log document.
5. **Globally serialized inference.** One process-global mutex protects the embedder and another the
   reranker. This protects the model but creates a hidden queue under concurrent traffic.
6. **Unbounded cross-request retrieval fan-out.** One semantic turn can issue up to
   `variants × retrieval sides` concurrent DocumentDB operations. `join_all` bounds one request but
   there is no global admission limit across clients.
7. **Unnecessary query-expansion calls.** With no history, `prepare_queries` still calls the model
   whenever multi-query is enabled, including short and exact-token questions.
8. **Late stream opening.** `/chat` completes routing, history loading, retrieval/reranking, and model
   stream creation before returning its SSE stream. The client receives no progress or cancellation
   point during most TTFT.
9. **Score is bounded, not calibrated.** Sigmoid converts reranker logits to `(0,1)`, but the fixed
   gate has not been calibrated against answerable and no-answer clinical queries.
10. **Fixed index shape.** The vector index uses IVF with `numLists=100` independent of corpus size.
11. **Unbounded history APIs.** Conversation/message list routes collect all matching documents; the
    UI renders all messages and reparses the growing Markdown answer during streaming.
12. **Expensive dashboard counts.** `/stats` groups the records collection to recompute distinct rows
    and tables on each request instead of using ingest-maintained summaries.

## Reference service-level objectives

Measure on named reference hardware with warmed models and a representative local corpus. Store the
hardware profile, model variants, corpus size, concurrency, and configuration with every report.

| Path                   |                   Metric |                                                 Initial gate |
| ---------------------- | -----------------------: | -----------------------------------------------------------: |
| Conversational         |                 p95 TTFT |                                                      < 1.0 s |
| Semantic, no history   |                 p95 TTFT |                                                      < 1.5 s |
| Semantic, with history |                 p95 TTFT |                                                      < 2.5 s |
| Structured aggregation |      p95 time to`rows` |                                                      < 2.5 s |
| Warm semantic request  |  p95 total server errors |                                                         < 1% |
| Retrieval              |               hit-rate@6 |                                  no regression from baseline |
| Retrieval              |                      MRR |                                  no regression from baseline |
| No-answer slice        | refusal precision/recall | explicitly reported; threshold selected from validation data |
| Structured             |       exact-result match |                               100% on deterministic fixtures |
| Router                 |           route accuracy |             ≥ 95% overall; 100% on safety-critical fixtures |
| Ingestion              |                 peak RSS |                         bounded as table row count increases |
| Ingestion              |           failed refresh |              previous complete generation remains searchable |
| Soak                   | 10/25/50 clients for 8 h |                      no unbounded RSS, queue, or task growth |

The values are first targets, not promises. Revise them only from recorded baseline data.

## Workstream A — Measurement and regression gates (P0)

**Dependency:** none. Do this before optimization work.

Implement plan 21 rather than creating a second evaluator:

1. Add stable `tracing` spans for `route`, `history`, `rewrite_expand`, `embed`, `search_vector`,
   `search_text`, `rrf`, `rerank`, `gate`, `prompt`, `ttft`, `generate`, `persist`, and the equivalent
   structured/NL-to-SQL stages.
2. Emit one structured request summary containing request id, route, cache hits, candidates in/out,
   token counts, per-stage milliseconds, TTFT, total time, outcome, and error class. Never log PHI,
   prompts, record text, JWTs, or source credentials.
3. Add in-process counters/histograms exposed to admins through `/metrics/summary`.
4. Build deterministic judge-free datasets for routing, retrieval, structured aggregation,
   no-answer behavior, and NL-to-SQL validation. Use the seeded development source.
5. Port local judged evaluation only after deterministic suites are stable. The judge must run through
   Foundry Local; evaluation records must remain on-premises.
6. Add CI for `cargo test --bin onprem-server`, both Rust builds/checks, `npx tsc --noEmit`, the Vite
   production build, and a small deterministic test tier. Add the Docker-backed smoke suite where a
   suitable runner is available.

**Primary files:** `onprem-rag-server/src/rag/routes.rs`, `retrieval/mod.rs`, `answer.rs`, `router/`,
`nl2sql/`, a new small `metrics.rs`, new `eval/`, and `.github/workflows/`.

**Gate:** the retrieval smoke suite runs in under two minutes; 100% of chat/agent requests produce a
summary record; deliberately disabling reranking causes a visible metric change.

## Workstream B — Bounded execution and overload behavior (P0)

**Dependency:** Workstream A baseline.

1. Add explicit admission limits for:
   - total active chat/agent generations;
   - DocumentDB searches;
   - embed batches;
   - rerank batches;
   - concurrent ingestion jobs per source and globally.
2. Store semaphores/queues in `AppState`; configure conservative defaults through `ONPREM_*` keys.
3. Apply queue-acquisition deadlines and per-stage timeouts. Return a clear `429` or `503` with a
   retry hint instead of waiting indefinitely.
4. Propagate client disconnect/cancellation through retrieval and generation where the SDK permits.
   Always release permits using owned guards/RAII.
5. Keep one model worker per device initially. Benchmark controlled multi-instance execution only when
   device memory allows it. Never remove the fastembed mutex without proving the underlying session is
   safe for concurrent mutation.
6. Consider short-window micro-batching for embedding/reranking only after queue metrics exist.

**Primary files:** `state.rs`, `config.rs`, `embed/mod.rs`, `retrieval/rerank.rs`, `retrieval/mod.rs`,
`rag/routes.rs`, `agents/routes.rs`, `ingest/routes.rs`, and `.env.example`.

**Gate:** a 50-client overload test has bounded queue depth and RSS; requests fail fast and clearly
when capacity is exhausted; cancelling clients do not leave work running indefinitely.

## Workstream C — Streaming, resumable ingestion (P0)

**Dependency:** Workstream A ingestion measurements; use Workstream B admission controls.

### C1. Page the source

Extend `SourceConnector` with a streaming/page API. Prefer stable keyset pagination by primary key;
use dialect-specific cursor/fetch support where keyset pagination is unavailable. Do not rely on large
`OFFSET` scans. Persist the last completed key/page in the job document.

### C2. Bound the pipeline

Use bounded channels with small capacity:

```text
source page -> sanitize/project -> chunk -> embed batch -> bulk write -> checkpoint
```

Overlap source I/O, inference, and DocumentDB writes while maintaining bounded memory. Start with an
embed batch of 32; benchmark 32/64/128 by device and keep it configurable.

### C3. Versioned cutover

Add `ingest_generation` to records and indexed-table metadata:

1. Allocate a new generation.
2. Write replacement records under that generation.
3. Validate row/vector counts and index queryability.
4. Atomically mark the generation active.
5. Delete the previous generation asynchronously after a grace period.

Queries filter to the active generation. A crash or partial table failure never removes the prior
complete generation. Startup recovery marks abandoned generations and makes cleanup resumable.

### C4. Efficient progress

Do not rewrite the full 500-entry log after every batch. Store counters separately and append bounded
log deltas, or update snapshots at a throttled interval (for example, once per second). Replace the
SSE route's 500 ms database polling with an in-process broadcast for live clients while keeping the
persisted job document as reconnect/recovery state.

**Primary files:** `connectors/mod.rs`, each SQL connector, `ingest/mod.rs`, `ingest/routes.rs`,
`documentdb/mod.rs`, `documentdb/vector.rs`, bridge ingestion commands, and the ingestion store.

**Gates:** ingest a corpus larger than available RAM without memory growth proportional to row count;
throughput improves by at least 1.5× on the reference host; disconnect/restart resumes safely; injected
failure at every stage leaves the prior generation searchable.

## Workstream D — Adaptive semantic fast path (P1)

**Dependencies:** Workstream A. Components can then land independently.

1. **Adaptive expansion:** skip the LLM expansion call for short keyword queries, quoted/exact terms,
   ICD/drug identifiers, strong conversational routes, and cache hits. Escalate to expansion when the
   router signals ambiguity or a cheap first pass has low confidence.
2. **Adaptive rerank depth:** choose 8–30 candidates using an RRF score elbow and configured minimum.
   Measure recall before reducing work. Add a batch-size setting and fast/accurate operating presets.
3. **Early SSE:** open the response after authentication/validation and emit `routing`, `rewriting`,
   `retrieving`, `reranking`, and `generating` status events. Preserve JSON encoding of token payloads.
4. **Result provenance:** extend `/search` hits with vector rank, text rank, fused score, rerank score,
   applied filters, and gate outcome so evaluation explains every selection.
5. **Cache policy:** report embedding/router cache hit rates. Version cache keys by model/config. Do not
   cache final PHI-bearing answers. Keep stale-data-sensitive retrieval results uncached initially.

**Primary files:** `rag/mod.rs`, `rag/routes.rs`, `retrieval/mod.rs`, `embed/mod.rs`, bridge event relay,
chat/agent activity strips, and `.env.example`.

**Gate:** the TTFT SLOs pass without regression in hit-rate@6, MRR, refusals, or citation correctness.

## Workstream E — Retrieval safety and context efficiency (P1)

Implement the unfinished items in plan 19:

1. Calibrate score-gate thresholds from relevant/irrelevant/no-answer score distributions. Sigmoid is
   only a monotonic transform, not probability calibration. Record model/version-specific thresholds.
2. Add allow-listed metadata filters for source, table, patient/entity id, code, and date range. Reject
   arbitrary client-supplied BSON operators. Verify filtered `cosmosSearch` against the local engine.
3. Deduplicate selected chunks by `(source_id, table, row_pk)` and fetch/merge parent-row context after
   reranking. Enforce per-row and total prompt token budgets.
4. Add deterministic citation validation and the optional post-stream local verifier described in
   plan 19. Verification must not delay the first answer token.
5. Tune weighted fusion only if the evaluation slices show a consistent deficit.

**Gate:** no patient mixing in filter fixtures; context stays inside its token budget; unsupported and
out-of-range citations are detected; the no-answer threshold meets the declared validation target.

## Workstream F — Index and read-path scaling (P1)

1. Benchmark IVF `numLists` and query parameters at representative corpus sizes. Compare against HNSW
   only after confirming the deployed DocumentDB engine supports the required syntax and behavior.
2. Record index kind, parameters, embedding model, dimensions, build time, peak RSS, recall, and p95
   query latency. Expose current index metadata to admins.
3. Add cursor pagination and hard limits to conversation, message, user, source, audit, and history
   listing endpoints. Preserve stable compound sort keys.
4. Replace `/stats` collection-wide grouping with counters/materialized summaries updated at generation
   cutover. Provide a repair/rebuild command for drift.
5. Review indexes using captured query shapes rather than adding speculative indexes.

**Primary files:** `documentdb/vector.rs`, `routes/conversations.rs`, `routes/stats.rs`, relevant list
routes, bridge response types, and React query screens.

**Gate:** read latency remains stable as conversations and records grow by 10×; dashboard load performs
bounded indexed queries; vector recall/latency results justify the chosen index configuration.

## Workstream G — Conversation memory and client efficiency (P2)

1. Implement plan 22's server-owned rolling summary plus token-budgeted verbatim tail. Memory is never
   a citation source; retrieved records remain authoritative.
2. Paginate message history and conversation lists. Load older messages on demand and virtualize only
   after profiling shows it is needed.
3. During streaming, avoid reparsing the entire accumulated answer as Markdown every animation frame.
   Render escaped/plain streaming text or parse at a lower cadence, then render full Markdown once the
   stream completes.
4. Throttle auto-scroll to animation frames and only pin to the bottom when the user is already near
   the bottom. Do not pull users away while reading earlier content.
5. Memoize completed Markdown/chart messages. Add performance fixtures for long answers, large tables,
   and 500-message conversations.
6. Retry the always-on log stream with capped exponential backoff and jitter. Consider pausing the relay
   when no console consumer is active if log volume is material.
7. Add bundle-size budgets and capture route chunk sizes in CI. Keep the existing lazy feature imports.

**Primary files:** server `routes/conversations.rs`, new memory module from plan 22, app chat/agent
message lists and bubbles, `components/Markdown.tsx`, `lib/bridgeEvents.ts`, and stores.

**Gate:** no long tasks above 50 ms during normal streaming on reference desktop hardware; a long
conversation remains responsive; user-controlled scroll position is preserved; route chunks stay
within recorded budgets.

## Workstream H — Production validation and capacity profiles (P2)

1. Create local load-test scenarios for conversational, semantic, structured, concurrent streaming,
   ingestion plus chat, reconnect storms, and administrative list endpoints.
2. Run failure injection: DocumentDB restart, source timeout, model load failure/OOM, dropped SSE
   client, corrupted/partial ingestion generation, and server restart during ingestion.
3. Publish capacity profiles per validated hardware configuration: model variants, execution providers,
   corpus size, safe concurrent generations/searches/ingests, p95 latency, and peak memory.
4. Add readiness separate from liveness. Readiness reports DocumentDB, required indexes, active model
   availability, warmup state, and queue saturation without exposing secrets.
5. Document backup/restore and disaster-recovery drills for DocumentDB, encrypted source credentials,
   model cache, and configuration keys.
6. Keep logs local, rotate them, redact sensitive fields, and define retention. No PHI-bearing payloads
   in tracing or metrics labels.

**Gate:** an eight-hour soak passes without monotonic memory/task growth; each injected dependency
failure degrades clearly and recovers; backup restoration is exercised rather than merely documented.

## Delivery order

```text
A  measurement + CI
   |
   +--> B  admission control/timeouts
   |      |
   |      +--> C  streaming/versioned ingestion
   |
   +--> D  adaptive RAG fast path
   |      |
   |      +--> E  retrieval safety/context
   |
   +--> F  index/read scaling
          |
          +--> G  memory/client scaling

A–G --> H production validation and capacity profiles
```

Suggested PR-sized sequence:

1. A1 tracing spans and request summary.
2. A2 judge-free retrieval/router smoke runner.
3. A3 CI and metrics summary.
4. B1 state-level semaphores, queue metrics, and timeouts.
5. C1 connector paging API plus one PostgreSQL implementation.
6. C2 bounded ingest pipeline for all connectors.
7. C3 generation cutover and recovery.
8. D1 adaptive expansion and rerank depth.
9. D2 early SSE status and cancellation.
10. E metadata filters, parent expansion, and calibrated gate.
11. F pagination, materialized stats, and index benchmark.
12. G memory compaction and profiled client changes.
13. H soak, failure injection, runbooks, and capacity profiles.

## Change discipline

Every performance PR must include:

- the hypothesis and affected stage;
- before/after measurements on named hardware;
- quality results for relevant eval slices;
- concurrency and memory behavior where applicable;
- rollback/config fallback;
- updated `.env.example` and bridge types when the API changes.

Do not merge an optimization that improves the mean while materially worsening p95/p99, grounding,
refusal behavior, citation correctness, or failure recovery.
