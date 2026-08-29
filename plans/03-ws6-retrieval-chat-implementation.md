# 03 — WS6 (Hybrid Retrieval + RAG Chat) implementation notes

What actually landed for Workstream 6, and where it deviates from the design in
[01-retrieval-design.md](01-retrieval-design.md). The pipeline shape matches 01; this note
records the concrete code + the deliberate simplifications.

## Server modules

- **`documentdb/vector.rs`** — added the two retrieval sides fused by RRF:
  - `vector_search(db, query_vector, k)` — `cosmosSearch` as pipeline stage 1, score via
    `$meta:"searchScore"`.
  - `text_search(db, query, limit)` — `find({$text:{$search}})` projected + sorted by
    `$meta:"textScore"`.
  - Both return `Vec<Hit>`; `Hit.fields` stays BSON and is converted to JSON only at the
    `Passage` boundary. `Hit.score` (raw per-side score) is carried but currently unread —
    kept for a future `/search` debug field / server-side RRF; hence the dead-code warning.
- **`retrieval/rrf.rs`** — `fuse(rankings, k)` RRF by rank (`1/(k+rank+1)`), k=60. Unit-tested.
- **`retrieval/rerank.rs`** — `fastembed` cross-encoder. **Verify-early flag resolved:** the
  variant is `RerankerModel::BGERerankerV2M3` and `rerank(query, docs, false, None)` returns
  `Vec<RerankResult>` with `.index` + `.score`. Process-global `OnceLock<Mutex<TextRerank>>`,
  all work in `spawn_blocking` (mirrors `embed/mod.rs`).
- **`retrieval/mod.rs`** — `retrieve(db, config, queries, mode, rerank_enabled, top_k)`:
  embed + vector-search each query (+ `$text` if Hybrid), dedup hits into a `HashMap<id,Hit>`
  with per-list id orderings, `rrf::fuse`, take top-N (`rerank_top_n` when reranking else
  `top_k`), rerank against `queries[0]`, return top-k `Passage`s. `Passage.score` is the
  rerank score when reranked, else the RRF fused score (`reranked` flag distinguishes them).
- **`rag/mod.rs`** — `rewrite_query` (history-aware), `expand_queries` (multi-query), the
  grounded `SYSTEM_PROMPT`, and `build_prompt` (1-based numbered context matching citation
  indices). Rewrite/expand use `FoundryManager::complete` (new; drains `generate_stream`).
- **`rag/routes.rs`** — `POST /search` (debug JSON: `query`, `queries`, `passages`) and
  `POST /chat` (SSE: `citations` → `token`* → `done`, `error` on mid-stream failure). Both
  accept per-request `mode`/`rerank`/`top_k` overrides (`#[serde(flatten)]` `RetrievalOpts`).

## Deliberate deviations from 01

- **Sequential, not parallel, retrieval.** 01 says "~50/side, parallel". The implementation
  runs the per-query vector/text searches sequentially (simple `for` loop). Correctness is
  identical; only latency differs. Parallelising with `futures::join`/`try_join_all` is a
  clean later optimisation if p95 latency needs it — noted, not done.
- **Rewrite/expand degrade gracefully.** If the chat model is slow/unavailable, both fall
  back to the raw query (logged `warn`) rather than erroring — retrieval must not hard-depend
  on LLM rewrite success.
- **Score gate is off by default.** `ONPREM_SCORE_GATE` unset → gate disabled. When set,
  `/chat` refuses (streams a "no relevant records" token, no generation) if the DB returned
  nothing *or* the top passage's rerank score is below the floor. `bge-reranker` scores aren't
  normalised, so the floor must be tuned against real data before enabling.
- **No metadata pre-filter yet.** 01's patient/date `$match` safety filter is not wired in
  this workstream — the generic schema has no fixed patient/date columns to key on. Revisit
  when a clinical schema mapping is added.

## Bridge + UI

- **Bridge:** `search` (returns `SearchResponse`) and `chat` (relays SSE → `chat://citations`
  JSON string, `chat://token`, `chat://error`, `chat://done`). Reuses the `chat://*` event
  namespace from the WS3 `generate` test — only one of Settings/Chat is mounted at a time, so
  no listener collision.
- **UI:** `chat/Chat.tsx` — message list, streamed assistant bubble, expandable citation
  chips (show source/row/chunk/score), and live `mode` (hybrid/vector) + `rerank` toggles.
  Wired as a new "Chat" tab in `App.tsx`.

## Gate status (WS6)

`cargo build` (both crates) + `npx tsc --noEmit` clean. End-to-end (`/search` ranking,
streamed cited `/chat`, toggle effects) still needs a live run against ingested records +
a loaded Foundry chat model — untested at the wire level here.
