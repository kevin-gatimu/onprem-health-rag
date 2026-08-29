# CLAUDE.md

Guidance for working in this repo. Read `plans/00-master-plan.md` for the full architecture and
`plans/01-retrieval-design.md` for the RAG/chat accuracy design — the `plans/` folder is the persistent
"mind cache" for this project; add numbered `NN-topic.md` design notes there as decisions are made.

## What this is

An **on-premises RAG system for health records**. Health data lives in operational SQL databases
(PostgreSQL, MySQL, SQL Server). The system ingests those records through an on-prem server that calls a
**local** embedding model, stores records + vector embeddings in DocumentDB, then lets users **chat** with
the data through a **local** chat LLM. Everything runs on-prem — no cloud model calls (PHI privacy).

## Components

- **`onprem-rag-app/`** — Tauri v2 desktop app.
  - `src/` — React 19 + Vite 7 + TypeScript frontend. Renders UI, manages state. Never calls the server or
    holds the JWT directly.
  - `src-tauri/` — Rust **bridge**. Holds the server base URL + JWT in managed state (`state.rs`), exposes
    `#[tauri::command]`s (`commands.rs`) that make the actual HTTP calls with `reqwest`, and relays server
    SSE streams to the frontend as Tauri events.
- **`onprem-rag-server/`** — Rust **Rocket 0.5** server. The workhorse: source-DB connectors, Foundry Local
  (chat + embeddings), hardware/execution-provider detection, DocumentDB storage + hybrid retrieval, auth.
  Network-exposed; async (Tokio) + stateless JWT so it serves many desktop clients.

**Traffic flow:** React → `invoke` → src-tauri bridge (reqwest, holds JWT) → server REST → DocumentDB /
Foundry / source DBs. Streaming: server SSE → bridge → Tauri events (`chat://token`, `ingest://progress`).

## Stack

- **Chat LLM / embeddings:** Foundry Local (`foundry-local-sdk` crate). Chat default
  `qwen2.5-7b-instruct-openvino-gpu:2` (swappable in Settings); embeddings `qwen3-embedding-0.6b` (1024
  dims, asymmetric — query gets the qwen3 instruction prefix, documents plain). EPs (GPU/NPU/CPU/OpenVINO)
  auto-detected; user overrides by loading a specific variant id.
- **Document store:** DocumentDB (MongoDB-wire, pgvector under the hood) via the `mongodb` 3.x crate. Vector
  search via the `cosmosSearch` operator; full-text via the GA legacy `$text` index.
- **Retrieval:** history-aware query rewrite → multi-query expansion (3) → hybrid (vector + `$text`, ~50/side)
  → RRF fuse (k=60, in Rust) → `fastembed` cross-encoder rerank (`bge-reranker-v2-m3`, top-N≈30 → top-k≈6) →
  grounded generation with a score gate. Chunking on by default (per-passage, ~384-token / 64 overlap).
- **Source connectors:** `sqlx` (Postgres, MySQL) + `tiberius` (SQL Server — sqlx dropped MSSQL).
- **Auth:** JWT (HS256, `jsonwebtoken`) + argon2 password hashing. Default `admin` / `password` seeded into
  DocumentDB on first boot. Enforced at DocumentDB creds, server JWT + Rocket request guard, and UI login.

## Configuration

All server config comes from the environment, prefix `ONPREM_` (see `.env.example`; `config.rs` loads it,
with `.env` read via `dotenvy` in dev). Copy `.env.example` → `.env` and adjust.

## Run / build

```bash
# 1. DocumentDB (required). Wait for "=== DocumentDB is ready ===".
docker compose up -d documentdb
# Optional seeded Postgres source for end-to-end testing:
docker compose --profile dev-sources up -d

# 2. Foundry Local (one-time): winget install Microsoft.FoundryLocal

# 3. Server
cd onprem-rag-server && cargo run           # serves http://0.0.0.0:8000, GET /health

# 4. Desktop app
cd onprem-rag-app && npm install && npm run tauri dev
```

Quick checks: `cargo build` in each Rust crate; `npx tsc --noEmit` in `onprem-rag-app`.

## Conventions

- Server routes return `Result<T, AppError>`; `AppError` (`error.rs`) serializes to a JSON body + HTTP status.
- Bridge commands return `Result<T, String>`; keep the JWT in Rust managed state, never in the web layer.
- Add server deps per workstream (not all up front) to keep each stage compiling — heavy crates
  (`fastembed`/`ort`, `foundry-local-sdk`) come online only when their workstream lands.
- Comments explain *why*, matching surrounding density. Match existing naming/idiom.

## Build order (workstreams — see master plan)

1. **Foundations** ✅ — docker-compose + `.env`; server `config`/`state`/`error`/`main` + `/health` +
   DocumentDB connect; Tauri bridge skeleton; app calls `/health` through the bridge.
2. **Auth** — argon2, admin seed, JWT, `AuthUser` guard, `/auth/*`; bridge login/me; React login + guard.
3. **Foundry + hardware** — manager lifecycle, `GET /hardware`, `GET /models` + `POST /models/select`; Settings UI.
4. **Connectors** — `SourceConnector` trait + PG/MySQL/MSSQL; `/sources` (+ test) w/ encrypted creds; Sources UI.
5. **Ingestion + indexes** — cosmosSearch + `$text` indexes; fetch → chunk → embed → upsert; `/ingest` SSE; UI.
6. **Hybrid retrieval + RAG chat** — vector + `$text` → RRF → rerank → grounded stream; `/search` + `/chat` SSE; Chat UI.
7. **Polish** — error surfaces, tracing, CORS/TLS notes, READMEs.
