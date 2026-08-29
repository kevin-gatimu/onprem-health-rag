# 20 — Performance Fast Path (latency & throughput)

> Status: IN PROGRESS. Goal: **fast** — cut time-to-first-token (TTFT) and total answer time on the
> default chat path without sacrificing the accuracy stack. Current default path costs 1–3 LLM
> calls (rewrite, expansion, generation) + embed + 2×(1+variants) DB searches + rerank before the
> first token streams. Companions: `17` (router already removes whole stages for conversational),
> `21` (latency instrumentation proves each win).
> Done: 20.1, 20.2, 20.3, 20.4. Skipped: 20.5, 20.6, 20.7, 20.8 (scope/eval gated).

## Measured shape of a default turn (from code survey)

| Stage | Cost | Notes |
|---|---|---|
| Query rewrite | 1 LLM call (GPU) | only when history present — already skipped on first turn |
| Multi-query expansion | 1 LLM call (GPU) | default ON, 3 variants |
| Embed query ×variants | ~ms (ONNX, CPU) | `spawn_blocking`, no cache |
| Vector + $text ×variants | 2×3 = 6 aggregations | sequential today |
| RRF | negligible | |
| Rerank top-30 | 0.5–1 s (ONNX) | `spawn_blocking` |
| Generation | streaming | TTFT dominated by everything above + prefill |

## Improvements (ordered)

### 20.1 Fold rewrite + expansion into ONE call — biggest single win [DONE]
- Replace the two sequential GPU calls with one `AgentKind::QueryRewrite` call returning a strict
  tool-call `rewrite { standalone: string, variants: [string; 2] }`. Same total tokens, one fewer
  round trip + one fewer prefill (~300–800 ms saved every history turn).
- No history: skip entirely (already) — but also skip *expansion* for short keyword-ish questions
  (< 4 words rarely benefit from paraphrase; heuristic in `rag/mod.rs`).
- Files: `rag/mod.rs` (`prepare_queries()` replaces `rewrite_query`+`expand_queries`),
  `foundry/mod.rs` (tool), config: `ONPREM_MULTI_QUERY_COUNT` still honored.

### 20.2 Parallelize retrieval fan-out [DONE]
- All (variant × side) searches are independent: `futures::future::join_all` over 6 aggregations,
  and embed all variants in **one** fastembed batch (it's batched anyway — one `spawn_blocking`
  instead of N). DocumentDB handles 6 concurrent cursors trivially.
- Files: `retrieval/mod.rs`. Expected: retrieval wall time ≈ slowest single search (~50–150 ms)
  instead of the sum.

### 20.3 Query/embedding LRU caches [DONE]
- `embed_query` LRU (normalized text → vector, 1024 entries ≈ 4 MB) — repeat and follow-up turns
  hit constantly because rewrite output converges. Invalidation: never (embeddings are pure).
- Router decision cache is plan 17; rerank/citation results are NOT cached (stale-data risk).
- Files: `embed/mod.rs` behind the existing `OnceLock`, `Mutex<LruCache>`.

### 20.4 Warm start (kill the first-request cliff) [DONE]
- Today embedder, reranker, and chat model all lazy-load on first use — first user question can pay
  minutes (model download) or tens of seconds (load). Add `ONPREM_WARMUP=true`: on boot, spawn a
  background task that (a) initializes fastembed embedder + reranker (1-text dummy calls),
  (b) resolves + loads the chat model variant and the classify/text2sql phi-4-mini on NPU, honoring
  the LRU cap, (c) logs readiness into the existing logstream so the app's System Setup screen shows
  warm state. Never blocks Rocket ignition — `/health` stays instant.
- Files: `main.rs` (spawn after ignite), `foundry/mod.rs` (reuse ensure-loaded), `embed/mod.rs`,
  `retrieval/rerank.rs` (expose `warmup()`).

### 20.5 Rerank budget tuning
- 30 candidates × cross-encoder is the second-largest fixed cost. Two knobs, no code risk:
  - Early-exit: after RRF, if candidate 10..30 fused scores trail candidate 1 by a large factor,
    rerank only the top-`min(30, plateau)` (score-elbow heuristic in `retrieval/mod.rs`).
  - `ONPREM_RERANK_BATCH` env passthrough to fastembed batch size (GPU/DirectML EP benefits).
- Optional (measure first, 21): `ONPREM_RERANK_MODEL=jina-reranker-v1-turbo-en`-class small model
  for a "fast" preset; keep v2-m3 the accurate default.

### 20.6 Vector index: HNSW option + numLists hygiene
- `vector-ivf`/`numLists:100` was sized for small corpora; recall/latency degrade as rows grow.
  This engine also supports `vector-hnsw` (`m`, `efConstruction`, `efSearch`): add
  `ONPREM_VECTOR_INDEX_KIND=ivf|hnsw` (+ params), default stays ivf until verified live
  (VERIFY-EARLY: create + query hnsw index against the local container, same discipline as 02).
  For ivf, recompute `numLists ≈ max(100, rows/1000)` at ingest completion and recreate the index
  when it drifts >4× (log, admin-triggerable, not automatic mid-day).
- Files: `documentdb/vector.rs`, `config.rs`, ingest completion hook.

### 20.7 SSE pipelining — stream earlier
- Emit `routed` (17) immediately, `citations` as soon as rerank lands (already), and start
  generation prefill **concurrently with the citation SSE write** rather than sequentially in the
  handler. Also flush a `status` event per stage (`retrieving`, `reranking`, `generating`) so
  perceived latency drops (the app's activity strip already renders these for /agents; extend
  `/chat`).
- Files: `rag/routes.rs` EventStream body, bridge relay (`chat://status`), Chat UI strip reuse.

### 20.8 Ingest throughput (secondary)
- Pipeline embed and insert: embed batch N+1 (`spawn_blocking`, CPU/NPU) while batch N inserts
  (async I/O) — simple two-slot pipeline with a `tokio::sync::mpsc(1)` channel; ~1.5–2× on
  embed-bound ingests. Raise `BATCH_SIZE` 32 → 64 after measuring memory.
- Files: `ingest/mod.rs`.

## Latency budget targets (measured via 21 instrumentation, warm system)

| Path | Today (est.) | Target |
|---|---|---|
| Conversational ("hi") | 3–8 s (full pipeline) | < 1 s (17 Tier 0) |
| Semantic, no history | ~2–4 s TTFT | < 1.5 s TTFT (20.2/20.3/20.5) |
| Semantic, with history | ~3–6 s TTFT | < 2.5 s TTFT (20.1) |
| Structured (DocDb) | plan+exec+narrate | < 2.5 s to `rows` event |
| First request after boot | up to minutes | warm at boot (20.4) |

## Order of work

20.1 → 20.2 → 20.3 (one PR-sized change each, immediate wins) → 20.4 warmup → 20.7 SSE status →
20.5 rerank budget → 20.6 HNSW (needs live verification) → 20.8 ingest pipeline.

Every change lands with before/after numbers from the plan-21 latency harness in the commit
message; no blind tuning.

## Risks

- Warmup vs LRU: booting three models must respect `max_resident_models` (NPU-resident phi-4-mini
  is exempt by design — verify it stays that way).
- HNSW build memory on large collections — build at ingest completion, log RSS, keep ivf fallback.
- Parallel fan-out multiplies DocumentDB load ×6 per turn — connection pool is fine, but add a
  server-wide `Semaphore` (e.g. 32 concurrent searches) so many simultaneous clients degrade
  gracefully.
