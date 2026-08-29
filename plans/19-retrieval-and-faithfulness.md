# 19 — Retrieval Quality & Faithfulness Improvements

> Status: PLANNED. The semantic path (hybrid vector+`$text` → RRF → bge-reranker-v2-m3 → grounded
> generation) is live and correct. This plan makes it **more precise, more faithful, and more
> useful for generation** without changing the architecture. Ordered by impact-per-effort.
> Companions: `20-performance-fast-path.md` (latency), `21-eval-harness.md` (proves each change).

## Current gaps (from code survey)

1. **Score gate ships disabled** (`ONPREM_SCORE_GATE` unset) — the anti-hallucination refusal never
   fires; low-relevance passages flow into the prompt.
2. **No metadata pre-filtering** — retrieval can blend patients/tables; the master plan calls the
   patient filter "a safety property" but `retrieval::retrieve` has no `$match` support.
3. **Chunks lose their row** — top-k 384-word chunks go to the prompt bare; the full row `fields`
   (patient, date, table) are stored but not shown to the model, hurting synthesis and citations.
4. **No dedupe/merge** — overlapping chunks of the same row can occupy several of the 6 context
   slots with near-identical text.
5. **Word-based chunking** approximates tokens; long clinical codes inflate real token counts.
6. **No answer verification** — the `Verify` AgentKind + phi-4-mini-reasoning NPU slot exist
   (router phase 1) but nothing calls them.
7. **Citations are free-form** `[n]` — the model can cite nothing or invent `[9]` with 6 passages.

## Improvements

### 19.1 Calibrated score gate (default ON) — small, highest safety ROI
- bge-reranker-v2-m3 emits raw logits; apply `sigmoid` in `rerank.rs` so scores are comparable
  probabilities, then gate at `ONPREM_SCORE_GATE=0.30` default (tune with 21's eval set).
- Behavior change: when the **best** passage < gate → stream the fixed "no relevant records found
  for this question" reply with empty citations (never generate from noise). When top-1 passes but
  trailing passages fall below `gate/2`, drop them (adaptive top-k: 6 is a cap, not a quota).
- Files: `retrieval/rerank.rs` (sigmoid), `retrieval/mod.rs` (gate + trim), `config.rs` default,
  `.env.example`. Gate: eval no-answer questions (21) refuse; answerable set unaffected.

### 19.2 Metadata filters + patient-safety pre-filter
- Extend `RetrievalOpts` with `filter: Option<Document>` (`source_id`, `table`, `fields.*` equality,
  date ranges). Vector side: cosmosSearch supports filtered search on this engine via a `filter`
  clause in the `$search` stage — verify live (same VERIFY-EARLY discipline as `02-…md`); fallback
  is post-`$match` with a larger k. `$text` side: plain conjunctive `find` filter.
- Router entity hints (plan 17 Tier 2) map to filters when unambiguous: a resolved patient id/name →
  `fields.patient_id` equality. This is the "never blend patients" property.
- Also the mechanism the Hybrid route (17 Phase C) uses: cohort `row_pk`s → `filter: { row_pk: { $in: … } }`.
- Files: `retrieval/mod.rs`, `documentdb/vector.rs` (knn + text accept filter), `rag/routes.rs`
  (accept `filter` in body), bridge/UI optional (Data-Explorer-style source/table picker in Chat).

### 19.3 Parent-row context expansion ("small-to-big")
- Retrieve by chunk (precision), generate from the **whole row** (context): after rerank, group
  top-k chunks by `(source_id, table, row_pk)`; for each group fetch sibling chunks (one indexed
  query) and reassemble the row's full text ordered by `chunk_index`, capped at
  `ONPREM_PARENT_MAX_TOKENS=1200` words per row; hard prompt budget `ONPREM_CONTEXT_MAX_TOKENS=4000`
  enforced greedily by rerank score.
- Passage header gains structured metadata the model can cite:
  `[3] (patients — Facility A PG, row 4711, admitted 2026-03-02) text…`.
- Dedupe falls out for free: one context block per row, overlaps merged via `chunk_index` overlap
  arithmetic (chunker's 64-word overlap is known).
- Files: `retrieval/mod.rs` (expand step), `rag/mod.rs` (`build_prompt` headers). Gate: eval
  faithfulness + answer-relevance up on multi-chunk rows; context tokens bounded.

### 19.4 Citation discipline + verification pass (optional, default OFF at first)
- **Prompt tightening**: require every factual sentence to end with `[n]`; forbid uncited claims;
  give a one-line few-shot exemplar in `SYSTEM_PROMPT`.
- **Post-stream citation check** (cheap, deterministic): scan the final answer for `[n]` tokens;
  strip/flag citations `> passages.len()`; if an answer contains **zero** citations and passages
  exist → emit a new SSE `verify { status: "uncited" }` event so the UI can badge the reply.
- **Verifier pass** (`ONPREM_VERIFY_ENABLED=false` initially): after `done`, feed
  answer + passages to `AgentKind::Verify` (phi-4-mini-reasoning, NPU, already spec'd) with a strict
  tool-call `verdict { supported: bool, unsupported_claims: [string] }`; emit `verify` SSE event.
  Runs *after* the answer streams — zero added time-to-answer; UI shows a "grounding check ✓/⚠"
  badge when it lands. Files: `answer.rs` or new `verify.rs`, `rag/routes.rs`, bridge event, small
  UI badge.

### 19.5 Token-accurate chunking (ingest-side, opt-in)
- Word-window chunking stays default (speed). Add `ONPREM_CHUNK_TOKENIZER=approx|hf` — `hf` uses the
  BGE-M3 tokenizer already bundled by fastembed (expose a count via `tokenizers` crate on the
  side) inside the existing `spawn_blocking` batch; real 384-token windows, honest overlap.
- Re-chunking requires re-ingest; note in Sources UI. Gate: eval retrieval hit-rate unchanged or up;
  ingest throughput regression < 25%.

### 19.6 `$text` phrase & code handling
- Clinical exact-tokens (ICD codes "E11.9", drug names) are the `$text` side's whole job, but the
  Porter stemmer mangles dotted codes. At query time, detect code-like tokens
  (regex `[A-Z]\d{2}(\.\d+)?`, all-caps drug tokens) and add a quoted phrase to the `$search`
  string (`"E11.9"`), which this engine treats as exact-match conjunct. Cheap, no index change.
- Files: `retrieval/mod.rs` (query builder) + unit tests.

### 19.7 Better fusion weighting (only if eval demands)
- RRF is unweighted; hybrid questions with strong exact-term signals may deserve a lexical boost.
  Optional `ONPREM_RRF_WEIGHTS=vector:1.0,text:1.0` multiplier per list. Implement last — measure
  with 21 first; do not tune blind.

## Order of work

1. 19.1 score gate (small) → 2. 19.2 filters (medium, unblocks 17C hybrid) → 3. 19.3 parent-row
(medium) → 4. 19.6 code phrases (small) → 5. 19.4 citation check then verifier (small + medium) →
6. 19.5 tokenizer chunking (medium, opt-in) → 7. 19.7 weights (only with eval evidence).

Each lands independently with its own `cargo test` fixtures and an eval run (plan 21) attached to
the PR/commit message. Config keys all mirrored in `.env.example`.

## Risks

- Filtered cosmosSearch syntax must be **verified live** against the local container before 19.2
  merges (same trap history as the original cosmosSearch work — see `02-…md`).
- Parent-row expansion can blow the context on wide tables → the per-row and total caps are hard
  limits, greedy by rerank score.
- Verifier adds an NPU call per answer — keep it post-stream and default-off until 21 shows the
  false-positive rate is tolerable.
