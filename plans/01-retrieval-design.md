# 01 — Retrieval & Chat Accuracy Design

Design note for the RAG `/chat` path. Stack: Foundry Local (`qwen2.5-7b-instruct-openvino-gpu:2` chat —
**chat only, no embeddings**), `fastembed` **BGE-M3** for embeddings (1024-dim, ONNX/ORT), DocumentDB
(`cosmosSearch` vector + `$text` full-text), RRF in Rust, `fastembed` cross-encoder rerank.

> **Embeddings (corrected 2026-08-23):** Foundry Local has no embedding models. Embeddings are produced by
> `fastembed` **BGE-M3** (ORT path, shares the reranker's stack, GPU/DirectML-capable). BGE-M3 dense
> retrieval is **prefix-free** — embed queries and documents identically; the old qwen3/E5 asymmetric
> instruction prefix no longer applies. See [[embeddings-not-in-foundry-local]].

> **Chat model (confirmed):** `qwen2.5-7b-instruct-openvino-gpu:2` — a 7B pinned to the OpenVINO-GPU
> variant. Chosen over phi-4-mini for more reliable multi-record synthesis/grounding. User-swappable in
> Settings; the full variant id pins the execution provider (bare alias `qwen2.5-7b` lets the SDK pick).

## Per-turn pipeline (the default accurate path)

1. **History-aware query rewrite** — condense chat history + new question into a standalone retrieval
   query via the chat LLM. Resolves pronouns/ellipsis in follow-ups. Highest-ROI, commonly skipped step.
2. **Multi-query expansion (default ON):** paraphrase the rewritten query 2–3 ways, retrieve each, and RRF
   all variants together. Improves recall on vaguely-worded questions; cheap on the local 7B.
3. **Hybrid retrieval (~50/side, parallel), per query variant:**
   - Vector (`cosmosSearch`) — semantic recall. Embed the query with BGE-M3 (same encoder as documents; no prefix).
   - Full-text (`$text`) — exact-term recall (drug names, ICD codes, lab values, proper nouns). Essential
     for clinical text where embeddings miss exact tokens.
   - Metadata pre-filter (patient, date range) via downstream `$match` when the query implies one — also a
     safety property (never blend patients).
4. **RRF fuse (k=60)** — always on; the correct way to merge non-comparable cosine/BM25 scores and to
   combine the multiple query variants into one candidate list.
5. **Cross-encoder rerank** (`bge-reranker-v2-m3`, top-N≈30 → top-k≈6) — biggest precision lever; jointly
   scores query+passage. Toggle for latency; default ON.
6. **Grounded generation:** system prompt = answer only from context, say "I don't know" otherwise, cite
   record ids. **Score gate**: if top rerank score < threshold, return "no relevant records" (anti-halluc).
   Low temperature (~0.1–0.2). Citations inline.

## Stack-specific gotchas

- **Embeddings are prefix-free (BGE-M3)**: embed queries and documents with the *same* encoder and no
  instruction prefix. (The asymmetric qwen3/E5 `query:`/`Instruct:` scheme does NOT apply to BGE-M3 and
  would hurt recall if added.) Encapsulate embedding in `embed/mod.rs` (fastembed, not Foundry).
- **Two ONNX models, one stack**: BGE-M3 embedder + `bge-reranker-v2-m3` cross-encoder both run on
  fastembed/ORT. Load each once at startup and reuse; first run downloads from HF (pre-stage for air-gap).
- **Model choice vs. grounding**: default chat model is now `qwen2.5-7b-instruct-openvino-gpu:2` (7B), which
  synthesizes multi-record answers more reliably than phi-4-mini (~3.8B). Still prefer few high-precision
  passages (reranked top-6) + a strict grounding prompt. phi-4-mini remains selectable for lower latency.

## Defaults now ON (were optional boosters)

- **Multi-query expansion**: generate 2–3 paraphrases of the (rewritten) query, retrieve each, RRF all
  variants together. Improves recall on vaguely-worded questions; cheap on the local 7B.
  Config: `ONPREM_MULTI_QUERY_ENABLED` / `ONPREM_MULTI_QUERY_COUNT` (default 3).
- **Chunking**: embed per passage, not per whole record — split long free-text notes into ~256–512-token
  windows (`ONPREM_CHUNK_SIZE_TOKENS=384`) with slight overlap (`ONPREM_CHUNK_OVERLAP_TOKENS=64`). Keep
  structured fields (patient, date, dx) as metadata for filtering + citations. Config: `ONPREM_CHUNK_ENABLED`.

## Request/Settings knobs

`mode: vector|hybrid`, `top_k`, `rerank on/off`, `rewrite on/off`, `multi_query on/off`, `chunk on/off`,
`score_gate`. Defaults: hybrid, rewrite on, multi-query on (3), rerank on, chunking on (384/64), top-k 6,
RRF k=60, per-side 50, rerank top-N 30.
