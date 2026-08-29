# Retrieval Pipeline

> The per-turn semantic RAG pipeline for chat: query rewrite through grounded generation with reranking. Companion docs: [`architecture-overview.md`](architecture-overview.md), [`accelerator-and-hardware-detection.md`](accelerator-and-hardware-detection.md), [`aggregation-aware-retrieval.md`](aggregation-aware-retrieval.md).

## Per-Turn Pipeline (Accurate Path)

The default accurate pipeline for `/chat` requests:

```
user message + chat history
  │
  ├─▶ 1. History-aware query rewrite
  │    Condense chat history + new question into a standalone retrieval query
  │    via the chat LLM. Resolves pronouns/ellipsis in follow-ups.
  │
  ├─▶ 2. Multi-query expansion (default ON)
  │    Paraphrase the rewritten query 2–3 ways, retrieve each, and RRF
  │    all variants together. Improves recall on vaguely-worded questions.
  │
  ├─▶ 3. Hybrid retrieval (~50/side, per query variant)
  │    ├─▶ Vector search (cosmosSearch): semantic recall
  │    ├─▶ Full-text search ($text): exact-term recall (drug names, ICD codes)
  │    └─▶ Metadata pre-filter: patient/date/etc. when query implies one
  │
  ├─▶ 4. RRF fuse (k=60, always on)
  │    Merge non-comparable cosine/BM25 scores and combine query variants
  │    into one candidate list using reciprocal rank fusion.
  │
  ├─▶ 5. Cross-encoder rerank (top-N≈30 → top-k≈6, toggle)
  │    bge-reranker-v2-m3 jointly scores query+passage.
  │    Biggest precision lever; default ON.
  │
  └─▶ 6. Grounded generation
     System prompt: answer only from context, cite record IDs.
     Score gate (optional): if top rerank score < threshold, return "no relevant records".
     Low temperature (~0.1–0.2). Citations inline in the response.
```

## Key Stages Explained

### 1. History-Aware Query Rewrite

**Purpose:** Convert a follow-up question with pronouns/references into a standalone query.

**Example:**
- User Q1: *"Tell me about the patient's blood pressure"*
- Q2: *"Was it consistently high?"* ← References the patient from Q1
- **Rewrite:** *"Was the patient's blood pressure consistently high?"*

**Implementation:** Use the chat LLM's `complete()` method (from `FoundryManager`) with a concise system prompt. Falls back to the raw query if rewrite fails (logged as `warn`).

### 2. Multi-Query Expansion

**Purpose:** Generate 2–3 paraphrases of the rewritten query and retrieve each.

**Why:** Improves recall for vaguely-worded questions; cheap on the local 7B model.

**Config:**
- `ONPREM_MULTI_QUERY_ENABLED` (default true)
- `ONPREM_MULTI_QUERY_COUNT` (default 3)

**Implementation:** Call `expand_queries()` in `rag/mod.rs`, which uses the chat LLM to generate variants. Degrade gracefully: if expansion fails, use the original query (logged `warn`).

### 3. Hybrid Retrieval

Two independent searches, each returning ~50 hits:

#### Vector Search (cosmosSearch)

- **Index:** `cosmosSearch` with `vector-ivf`, 100 lists, cosine similarity, 1024 dims
- **Query:** embed the query text with **BGE-M3** (same encoder as documents, no instruction prefix)
- **Stage 1 of aggregation pipeline:** `{ $search: { cosmosSearch: { vector: [...1024 floats], path: "contentVector", k: 30 } } }`
- **Score:** `$meta: "searchScore"`
- **Semantic recall:** embeddings capture meaning, not exact words

#### Full-Text Search (`$text`)

- **Index:** legacy `$text` index (GA, on-prem-safe default)
- **Query:** direct `find({$text: {$search: "term phrase"}})` 
- **Score:** `$meta: "textScore"` (BM25-like ranking)
- **Exact recall:** essential for clinical terms (drug names, ICD codes, lab values, proper nouns) where embeddings miss exact tokens

#### Metadata Pre-Filter (Optional)

- Patient/date/clinic filters via downstream `$match` when the query implies one
- Safety property: never blend patients in multi-patient cohorts
- Not yet wired; revisit when clinical schema mapping is added

### 4. RRF Fuse (Reciprocal Rank Fusion)

**Why:** The two searches have incomparable scores (cosine vs. BM25). RRF combines them fairly.

**Formula:** For each unique document, compute `Σ 1/(k + rank_i)` where `k=60`, summing across both rankings.

**Implementation:** `retrieval/rrf.rs`; operates on a `HashMap<id, Hit>` populated from vector + text hits.

**Behavior:**
- Documents appearing in both lists score higher (reinforcement)
- Query expansion variants also fuse together via RRF (same mechanism)
- All candidates go to the next stage (reranking or top-k selection)

### 5. Cross-Encoder Rerank

**Model:** `bge-reranker-v2-m3` (fastembed, on-prem/air-gappable)

**Flow:**
- Take top-N candidates (~30) from RRF
- **Rerank** to top-k (~6)
- Score is the joint query+passage relevance (most precise signal)

**Process-global:** `OnceLock<Mutex<TextRerank>>`; all work in `spawn_blocking` (fastembed is sync).

**Toggle:** Can be disabled per-request via `rerank: false`; use when latency matters more than precision.

**Result:** `RerankResult { index, score }` where `index` is the position in the original batch, and `score` is not normalized (so threshold must be tuned against real data).

### 6. Grounded Generation

**System Prompt:**
- Answer only using the provided context.
- If the context does not contain the answer, say *"I don't know."*
- Cite the source by record ID inline (e.g., "As documented in record [id-12], ...").

**Score Gate (Optional):**
- If `ONPREM_SCORE_GATE` is set and the top passage's rerank score falls below the threshold, refuse to generate.
- Returns: *"No relevant records found."*
- Safety property: avoids hallucination when retrieval fails.

**Temperature:** ~0.1–0.2 (low; exact grounding, not creative).

**Citations:** Inline and **not** guaranteed to be exhaustive; user can expand via `/search` debug endpoint to inspect all retrieved candidates.

## Stack-Specific Design Decisions

### Embeddings: BGE-M3 (Prefix-Free, Not Asymmetric)

- **Model:** fastembed BGE-M3 (1024-dim, ORT)
- **Critical:** Embed queries and documents with the **same** encoder; no instruction prefix
- **What NOT to do:** The old qwen3/E5 `query:` / `Instruct:` scheme does NOT apply to BGE-M3. Adding an instruction prefix **hurts recall** and breaks the model's intent.
- **Encapsulation:** All embedding logic lives in `embed/mod.rs` (fastembed, not Foundry)

### Two ONNX Models, One Stack

Both BGE-M3 embedder and `bge-reranker-v2-m3` cross-encoder run on fastembed/ORT. Each is loaded once at startup and reused via `OnceLock<Mutex<...>>`. For air-gap deployments, pre-stage both ONNX files and tokenizers; `try_new_from_user_defined` loads them locally.

### Chat Model: Qwen2.5 7B (Not Phi-4-Mini)

- **Default:** `qwen2.5-7b-instruct-openvino-gpu:2` (pinned to OpenVINO-GPU variant)
- **Why:** Synthesizes multi-record answers more reliably than phi-4-mini (~3.8B)
- **Alternative:** `phi-4-mini` remains selectable for lower latency; trade-off is grounding quality
- **Config:** `ONPREM_CHAT_MODEL` (env var or Settings)

### Defaults (Now ON by Default)

#### Multi-Query Expansion

Generate 2–3 paraphrases of the rewritten query, retrieve each, RRF all variants together.
- Config: `ONPREM_MULTI_QUERY_ENABLED` (default true) / `ONPREM_MULTI_QUERY_COUNT` (default 3)
- Improves recall on vague questions; cheap on the local 7B

#### Chunking

Embed per passage, not per whole record. Split long free-text notes into ~256–512-token windows with slight overlap; keep structured fields as metadata for filtering + citations.
- Config: `ONPREM_CHUNK_ENABLED` (default true) / `ONPREM_CHUNK_SIZE_TOKENS` (default 384) / `ONPREM_CHUNK_OVERLAP_TOKENS` (default 64)

## Request Knobs (Toggleable)

The `/chat` endpoint accepts per-request overrides (via `RetrievalOpts` struct, `#[serde(flatten)]`):

| Option | Type | Default | Purpose |
|--------|------|---------|---------|
| `mode` | `vector` \| `hybrid` | `hybrid` | Vector-only or vector + full-text |
| `top_k` | integer | 6 | Final number of citations |
| `rerank` | bool | true | Enable cross-encoder reranking |
| `rewrite` | bool | true | Enable history-aware query rewrite |
| `multi_query` | bool | true | Enable multi-query expansion |
| `chunk` | bool | true | Enable per-passage chunking |
| `score_gate` | float \| null | null | Minimum rerank score; refuse if below |

Additional tuning parameters (env-only):
- `ONPREM_RRF_K` = 60 (RRF fusion parameter)
- `ONPREM_RERANK_TOP_N` = 30 (candidates passed to reranker)
- `ONPREM_VECTOR_LIMIT` / `ONPREM_TEXT_LIMIT` = ~50 each (per-side search limits)

## Implementation Details

### Retrieval Module Layout

```
onprem-rag-server/src/retrieval/
  mod.rs        — retrieve() entry point, dispatcher
  rrf.rs        — RRF fusion logic
  rerank.rs     — fastembed cross-encoder
```

### Vector Search (`documentdb/vector.rs`)

```rust
pub async fn vector_search(
    db: &Database,
    query_vector: Vec<f32>,
    k: u32,
) -> Result<Vec<Hit>>
```

Returns hits with `id`, `text`, `fields`, `source_id`, `score` (raw cosmosSearch score).

### Full-Text Search (`documentdb/vector.rs`)

```rust
pub async fn text_search(
    db: &Database,
    query: &str,
    limit: u32,
) -> Result<Vec<Hit>>
```

Returns hits sorted by `$meta: "textScore"`.

### Routing & Modes

- **Vector mode:** vector search only, skip full-text
- **Hybrid mode (default):** vector + full-text, RRF together
- **Future:** analytical mode (aggregation-aware retrieval, see `aggregation-aware-retrieval.md`)

### Error Handling & Graceful Degradation

- Rewrite fails → use original query (logged `warn`)
- Expansion fails → use original query (logged `warn`)
- Reranking unavailable → skip reranking, use RRF scores (logged `warn`)
- Both searches fail → return empty result (retrieval error, logged)

---

Source: synthesized from `plans/01-retrieval-design.md` and `plans/03-ws6-retrieval-chat-implementation.md`
