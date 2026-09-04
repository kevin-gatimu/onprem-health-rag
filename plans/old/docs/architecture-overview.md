# Architecture Overview

> **Superseded.** This file is retained for link/history stability. The authoritative replacement is [Architecture and Data Model](architecture-and-data-model.md). Do not treat the statuses, defaults, or diagrams below as current.

> Complete system topology: components, traffic flow, stack, data model, and API surface. Reference for new engineers joining the project. Companion docs: [`app-feature-surface-and-requirements.md`](app-feature-surface-and-requirements.md), [`retrieval-pipeline.md`](retrieval-pipeline.md), [`accelerator-and-hardware-detection.md`](accelerator-and-hardware-detection.md).

## System Context

**On-premises RAG system for health records.** Health data lives in operational SQL databases (PostgreSQL, MySQL, SQL Server). The system ingests those records through an on-prem server that calls a **local** embedding model, stores records + vector embeddings in DocumentDB, then lets users **chat with the data** through a local chat LLM. Everything runs on-prem — no cloud model calls — which is essential for PHI/privacy.

## Topology

```
┌─────────────────────────────────────────┐
│  Many desktop clients (Tauri app)       │
│  - React 19 + Vite 7 frontend           │
│  - Rust bridge (Tauri)                  │
└────────────────┬────────────────────────┘
                 │ REST + SSE (HTTP)
                 ▼
┌─────────────────────────────────────────┐
│  One server host                        │
│  - Rocket 0.5 server (Rust)             │
│  - Foundry Local (in-process)           │
│  - fastembed (ORT)                      │
│  - DocumentDB (Docker container)        │
└─────────────────────────────────────────┘
```

**Auth:** stateless JWT (HS256, `jsonwebtoken` crate). Server holds credentials at three layers: DocumentDB connection auth, server-side JWT + Rocket request guards, and UI login gate. Default credentials (`admin` / `password`) seeded into DocumentDB on first boot via argon2.

## Components

### 1. Desktop App (`onprem-rag-app/`)

**Frontend** — React 19 + Vite 7 + TypeScript
- Renders UI, manages local state
- Never calls the server directly or holds JWT
- Screens: Login → Settings/Hardware → Sources → Chat

**Bridge** — Rust (Tauri v2)
- Holds server base URL + JWT in **managed state** (`state.rs`)
- Exposes `#[tauri::command]`s (`commands.rs`) that make HTTP calls via `reqwest`
- Relays server SSE streams to frontend as Tauri events (`chat://token`, `ingest://progress`, `logs://line`)

### 2. Server (`onprem-rag-server/`)

Rust **Rocket 0.5** HTTP server. The workhorse layer:
- Source-DB connectors (PostgreSQL, MySQL, SQL Server)
- **Foundry Local** (chat model only; embeddings are fastembed BGE-M3)
- Hardware/execution-provider (EP) detection
- DocumentDB storage + hybrid retrieval (vector + full-text)
- Auth (JWT + argon2)
- Ingestion pipeline (fetch → chunk → embed → upsert)
- RAG pipeline (rewrite → expand → retrieve → rerank → generate)
- Live log streaming to clients

Network-exposed; async (Tokio) + stateless JWT, so it serves many concurrent desktop clients.

### 3. Foundry Local

**Microsoft Foundry Local** (preview; native in-process ONNX Runtime via `foundry-local-sdk` crate):
- **Chat models only** — all 36 aliases in the catalog are chat/reasoning/vision
- **⚠️ No embedding models** — Foundry has zero embedding models in its catalog
- Execution providers (EPs): CPU, WebGPU, OpenVINO, CUDA, TensorRT, QNN, Vitis AI (auto-detected; user override by loading a specific variant id)
- Embedded OpenAI server (dynamic port; discovered at runtime via `manager.urls()`)
- Default chat model: **`qwen2.5-7b-instruct-openvino-gpu:2`** (7B; chosen over phi-4-mini for better multi-record grounding)

Installation: `winget install Microsoft.FoundryLocal`

### 4. Embeddings & Reranking (fastembed + ORT)

**Not in Foundry.** The `fastembed` crate (ONNX Runtime under the hood, fully on-prem/air-gappable):
- **Embedding model:** BGE-M3 (1024-dim, prefix-free — same encoder for queries and documents, no instruction prefix)
- **Reranking model:** `bge-reranker-v2-m3` (cross-encoder, top-N≈30 → top-k≈6)
- Process-global `OnceLock<Mutex<TextEmbedding>>` + `OnceLock<Mutex<TextRerank>>` (both `Send`; all work in `spawn_blocking` since fastembed is sync)

### 5. DocumentDB (MongoDB-wire, pgvector)

**Docker image:** `ghcr.io/documentdb/documentdb/documentdb-local:latest` (port 10260)
- Auth: SCRAM-SHA-256 (self-signed TLS)
- Conn string: `mongodb://<user>:<pass>@<host>:10260/?tls=true&tlsAllowInvalidCertificates=true&retrywrites=false`
- Rust driver: `mongodb` 3.x crate
- **Vector search:** `createIndexes` with `cosmosSearch` operator; query via `$search.cosmosSearch` stage
- **Full-text search:** GA legacy `$text` index (on-prem-safe default)
- **Note:** no native `$rankFusion` on this engine — RRF fusing happens in Rust (`k=60`)

## Traffic Flow

```
React component
  │
  ├─▶ invoke() (Tauri command)
  │
  ▼
Tauri bridge command
  │
  ├─▶ reqwest call (with Bearer JWT from Rust state)
  │
  ▼
Rocket server endpoint
  │
  ├─▶ AuthUser guard (validates JWT)
  ├─▶ DocumentDB queries
  ├─▶ Foundry Local chat generation
  ├─▶ fastembed embedding/reranking
  ├─▶ Connector queries (PG/MySQL/MSSQL)
  │
  ▼
HTTP response or SSE stream
  │
  ├─▶ SSE events relayed via Tauri event bus
  │
  ▼
React component (useEffect listener)
```

## Collections (DocumentDB)

| Collection | Schema | Purpose |
|------------|--------|---------|
| **users** | `{ _id, username, password_hash, role, created_at }` | Auth; seeded with `admin` / `password` (argon2-hashed) |
| **sources** | `{ _id, name, kind, host, port, database, username, password_enc, query/tables, created_by }` | Saved source connections; credentials encrypted at rest (AES-256-GCM) |
| **records** | `{ _id, source_id, row_pk, fields{}, text, contentVector:[f32;1024], metadata, ingested_at }` | The ingested data; cosmosSearch vector index + `$text` index on `text` field |
| **jobs** | `{ _id, source_id, status, total, processed, errors[], started_at, finished_at }` | Ingestion job tracking |

**Record IDs:** `_id` fields use UUID v7 (time-ordered, better for B-tree locality than v4). Record document `_id` is a composite string: `{source_id}:{pk}:{chunk}`.

## REST API

All endpoints except `GET /health` and `POST /auth/login` require Bearer JWT.

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/health` | GET | Server health check |
| `/auth/login` | POST | Authenticate (user/pass → JWT) |
| `/auth/me` | GET | Current user info |
| `/auth/users` | POST | Create user (admin only) |
| `/hardware` | GET | Detected hardware + Foundry EPs + registration status |
| `/hardware/register-eps` | POST | Register accelerators into Foundry (admin only) |
| `/models` | GET | Catalog + cached/loaded state |
| `/models/select` | POST | Load a specific model variant (admin only) |
| `/sources` | GET | Saved source connections |
| `/sources` | POST | Save a new source (admin only) |
| `/sources/test` | POST | Test a source connection (admin only) |
| `/ingest` | POST | Start ingestion job (admin only) |
| `/ingest/<job>/stream` | GET | SSE stream of ingestion progress |
| `/chat` | POST | RAG chat (SSE; supports mode/rerank/top_k overrides) |
| `/search` | POST | Debug search (returns passages before generation) |
| `/logs/stream` | GET | SSE stream of server trace logs (filtered by category) |

## Server Modules (`onprem-rag-server/src/`)

| Module | Purpose |
|--------|---------|
| `main.rs` | Rocket launch, route mounting, `.manage(AppState)`, CORS, bind `0.0.0.0:8000` |
| `config.rs` | Environment config loading (prefix `ONPREM_`) |
| `state.rs` | `AppState` (Rocket managed state) |
| `error.rs` | `AppError` → HTTP responder |
| `auth/` | JWT, argon2, `AuthUser` guard, seed, routes |
| `documentdb/` | Connect, collections, vector.rs (cosmosSearch + `$text` indexes), helpers |
| `foundry/` | Manager lifecycle, hardware.rs (EP detection), chat.rs (streaming), routes |
| `embed/` | fastembed BGE-M3 (embedding + batch) |
| `retrieval/` | vector + full-text search, RRF fusion, reranking |
| `connectors/` | `SourceConnector` trait + PG/MySQL/MSSQL implementations |
| `ingest/` | Fetch → chunk → embed → upsert pipeline, job tracking, SSE |
| `rag/` | Query rewrite, prompt building, routes |
| `logstream.rs` | Tracing hub for live log relay |
| `routes/health.rs` | Health endpoint |

## Tauri Bridge (`onprem-rag-app/src-tauri/src/`)

Commands available to the frontend (all wrapped in `Result<T, String>`):
- `login`, `me`, `logout`
- `get_hardware`, `list_models`, `select_model`, `register_eps`
- `list_sources`, `test_source`, `save_source`
- `start_ingest`
- `search`, `chat`, `generate` (SSE)
- `start_log_stream`

Bridge holds JWT in Rust managed state; frontend never sees it.

## Configuration

All server config comes from the environment (prefix `ONPREM_`). See `.env.example`:
- `ONPREM_SERVER_PORT` (default 8000)
- `ONPREM_DOCUMENTDB_HOST`, `_PORT`, `_USERNAME`, `_PASSWORD`
- `ONPREM_CHAT_MODEL` (default `qwen2.5-7b`, bare alias for auto-EP resolution)
- `ONPREM_JWT_SECRET`
- `ONPREM_CREDENTIALS_KEY` (for source credential encryption)
- `ONPREM_CHUNK_SIZE_TOKENS`, `ONPREM_CHUNK_OVERLAP_TOKENS`
- `ONPREM_MULTI_QUERY_COUNT`, `ONPREM_MULTI_QUERY_ENABLED`
- `ONPREM_SCORE_GATE` (grounding safety gate threshold)
- `RUST_LOG` (tracing filter, default `info,onprem_server=debug`)

## Build Order (Workstreams)

1. **Foundations** ✅ — docker-compose + `.env.example`; Rocket server skeleton; DocumentDB connect; Tauri bridge; `/health` round-trip
2. **Auth** ✅ — argon2, admin seed, JWT, `/auth/*` routes, bridge login/logout, React login gate
3. **Foundry + hardware** ✅ — EP detection, model catalog, model selection, `/hardware` + `/models` routes, Settings UI
4. **Connectors** ✅ — PG/MySQL/MSSQL via sqlx + tiberius, source test, source save, credential encryption
5. **Ingestion + indexes** ✅ — cosmosSearch + `$text` indexes, fetch → chunk → embed → upsert, `/ingest` SSE, progress UI
6. **Hybrid retrieval + RAG chat** ✅ — vector + `$text` → RRF → rerank → grounded generation, `/chat` SSE, Chat UI
7. **Polish** — error surfacing, tracing completeness, CORS/TLS docs, READMEs

Current: workstreams 1–6 complete; log streaming (in 6) shipped; hardware + EP registration (in 3) refined.

## Dependencies

**Server:** rocket (0.5), tokio, mongodb (3.x), foundry-local-sdk (+winml on Windows), sqlx (0.8), tiberius, serde/serde_json, jsonwebtoken, argon2, reqwest (0.12), fastembed (6.x), async-trait, uuid (v7 feature), chrono, futures, tracing/tracing-subscriber, rocket_cors, eventsource-stream

**Bridge:** reqwest, tokio, futures-util, eventsource-stream, tauri

**Frontend:** react, react-router-dom, @tanstack/react-query, zustand, tailwind

---

Source: synthesized from `plans/00-master-plan.md`
