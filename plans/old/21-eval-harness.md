# 21 — Evaluation Harness & Instrumentation (measure everything)

> Status: PLANNED. Plans 17–20 each claim accuracy or latency wins; this plan builds the yardstick
> so every change ships with numbers. Two parts: (A) port/extend the RAGAs eval from the old
> Electron project to this stack, (B) per-stage latency + quality instrumentation inside the server.
> Principle from the share-chat design input: **instrument the two paths separately** — semantic is
> precision-via-reranking (log retrieval hit rate), structured is precision-via-schema-linking (log
> SQL/plan execution success rate). One shared metric would hide regressions.

## A. Offline eval suite (`eval/` at repo root)

### A.1 Reuse what exists
The Electron project already has a working harness
(`On-premise-Rag-system-for-Health-Records/RAGAs-Eval/`): 12-question clinical set + RAGAs metrics
(faithfulness, answer relevance, context recall/precision), Foundry-Local judge, config ablations
(full / naive / no-rerank), checkpoint-resume, appendix table builder. Port the **capture layer**
to hit this server's REST API (`POST /chat`, `POST /search`, `POST /agents/*`) with a JWT — the
scoring layer (Python, RAGAs) is stack-agnostic and moves nearly unchanged.

### A.2 Datasets (extend from 12 → ~60 questions, JSONL, seeded corpus)
| Slice | n | Purpose |
|---|---|---|
| Semantic lookup/narrative | 15 | existing style; reference answers + reference doc ids |
| Exact-token (ICD codes, drug names) | 8 | proves `$text` + 19.6 phrase handling |
| Structured DocDb (count/group/trend) | 10 | expected numeric results, judge-free |
| Text-to-SQL (7 classes × dialect focus) | 14 | expected SQL result sets vs hand-written SQL (plan 18 gates) |
| Hybrid (cohort + summarize) | 5 | plan 17C |
| No-answer (out-of-corpus) | 5 | must refuse — score gate 19.1 |
| Conversational | 3 | must NOT retrieve; latency < 1 s |
- All against the seeded dev-sources corpus (deterministic SEED, like the old Facility-A set).
  Store under `eval/data/*.jsonl` with `route_expected`, `intent_expected` fields for router scoring.

### A.3 Metrics per path
- **Router**: classification accuracy vs `route_expected` (per tier), tier distribution, cache hits.
- **Semantic retrieval** (judge-free, fast): hit-rate@k / MRR on reference doc ids from `/search`
  (no LLM judge needed — run on every PR).
- **Generation** (judged, slow): RAGAs faithfulness + answer relevance via local judge — run on
  demand / nightly.
- **Structured + SQL**: execution success rate, exact-result match vs hand-written SQL, validation
  rejection rate (adversarial fixtures must be 100% rejected).
- **Latency**: per-stage timings scraped from the server run log (see B) — TTFT, total, per stage.

### A.4 Runner
- `eval/run.mjs` (Node, mirrors old `scripts/run-eval.mjs` ergonomics): `--suite retrieval|router|
  sql|full`, `--config full|naive|no-rerank|fast`, logs into `eval/reports/<date>-<suite>.md` + CSV.
  Config presets map to `/chat` body overrides (mode, rerank, top_k) so no server restarts needed.
- CI-lite: a `--smoke` mode (5 questions, judge-free) fast enough to run before every merge.

## B. Server instrumentation

### B.1 Per-stage tracing spans
- Wrap each pipeline stage in a `tracing` span with a stable name + timing field:
  `route`, `rewrite_expand`, `embed`, `search_vector`, `search_text`, `rrf`, `rerank`, `gate`,
  `prompt`, `ttft`, `generate`, and on the structured side `plan`, `validate`, `execute`, `narrate`,
  plus `nl2sql.link/generate/validate/execute/repair`.
- One summary line per request at info level (JSON-ish fields): route, tier, cached, per-stage ms,
  candidates in/out, rerank top score, gated bool, tokens out. This is what the eval runner scrapes
  and what operators grep in the existing logstream UI.
- Files: `rag/routes.rs`, `retrieval/mod.rs`, `answer.rs`, `router/`, `nl2sql/` — pure
  `tracing::info_span!`/fields, no new deps.

### B.2 Quality counters (in-process, surfaced via `/health`-style route)
- `GET /metrics/summary` (admin): rolling counters since boot — requests per route class, structured
  fallback count (structured→semantic silently degrades **today with no metric**, hiding
  regressions), score-gate refusals, rerank score p50/p95, SQL validation rejections, repair-pass
  invocations, cache hit rates. Plain JSON; Prometheus format is out of scope (on-prem desktop
  product, no scrape infra assumed).
- Files: small `metrics.rs` (`AtomicU64`s + a `Mutex<Histogram>>`-lite), touched at the same call
  sites as B.1; route in `routes/`.

### B.3 Retrieval debug endpoint upgrade
- `POST /search` already exists; extend response with per-stage provenance: each hit carries
  `{ vector_rank, text_rank, fused_score, rerank_score, gated }` so eval and the Data Explorer can
  show *why* a passage won. Files: `retrieval/mod.rs` (thread ranks through), `rag/routes.rs`.

## Order of work

1. B.1 spans + summary line (small — do FIRST, plans 17–20 all depend on before/after numbers).
2. A.1/A.4 port capture runner + A.2 judge-free retrieval slice (hit-rate@k on the seeded corpus).
3. B.2 counters + B.3 search provenance.
4. A.2 remaining slices as their features land (router slice with 17, SQL slice with 18, …).
5. Judged RAGAs runs wired last (needs stable judge model choice on this machine's EPs).

## Gates

- `eval/run.mjs --suite retrieval --smoke` runs green against docker dev-sources in < 2 min.
- A deliberate regression (rerank off) visibly drops hit-rate@6 and RAGAs faithfulness in reports.
- Summary log line present for 100% of chat/agent requests; eval runner parses it without scraping
  ad-hoc text.
- Structured-fallback counter proves >0 on a forced plan failure (kill catalog), 0 in the happy path.
