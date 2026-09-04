# CLAUDE.md

On-premises RAG for health records. Authoritative documentation starts at
`plans/docs/README.md`. Archived historical plans are in `plans/old/`; add new current
documentation under `plans/docs/`, not as numbered root-plan files.

## What this is

PHI never leaves the premises — **no cloud model calls**. Records in operational SQL databases
(PostgreSQL, MySQL, SQL Server) are ingested by an on-prem server that embeds them **locally**,
stores records + vectors in DocumentDB, and lets users **chat** with the data through a **local** LLM.

## Components

- **`onprem-rag-app/`** — Tauri v2 desktop / Android client.
  - `src/` — React 19 + Vite + TypeScript frontend. Renders UI; never calls the server or holds the JWT.
  - `src-tauri/` — Rust bridge. Holds base URL + JWT in managed state (`state.rs`), exposes
    `#[tauri::command]`s (`commands.rs`) that make the `reqwest` calls, and relays server SSE to the
    frontend as Tauri events.
- **`onprem-rag-server/`** — Rust **Rocket 0.5** server (async Tokio, stateless JWT). Connectors,
  Foundry Local (chat), fastembed (embed + rerank), hardware/EP detection, DocumentDB + retrieval, auth.

**Flow:** React → `invoke` → bridge (holds JWT) → server REST → DocumentDB / Foundry / source DBs.
Streaming: server SSE → bridge → Tauri events (`chat://token`, `ingest://progress`).

## Stack

- **Chat:** Foundry Local (`foundry-local-sdk`), default `qwen3-8b` (swappable in Settings). EPs
  (GPU/NPU/CPU/OpenVINO) auto-detected; a task-aware model router places per-role models on devices.
- **Embeddings:** fastembed **BGE-M3** (`bge-m3`, 1024-dim, prefix-free) — *not* Foundry (its catalog has none).
- **Store:** DocumentDB (MongoDB-wire, pgvector) via `mongodb` 3.x. Vector: `cosmosSearch`; full-text: `$text`.
- **Retrieval:** query rewrite → multi-query (3) → hybrid (vector + `$text`) → RRF (k=60) → fastembed
  rerank (`bge-reranker-v2-m3`, ~30→6) → grounded generation with a score gate. Chunking ~384/64 tokens.
- **Routing:** tiered intent router (`router/`) separates structured (aggregation) from semantic (retrieval) queries.
- **Connectors:** `sqlx` (Postgres/MySQL) + `tiberius` (SQL Server).
- **Auth:** JWT (HS256, `jsonwebtoken`) + argon2. Default `admin` / `password` seeded on first boot.

## Configuration

Server config is env-driven, prefix `ONPREM_` (`config.rs`; `.env` read via `dotenvy` in dev).
Copy `.env.example` → `.env` and adjust.

## Run / build

```bash
docker compose up -d documentdb          # required; optional sources: --profile dev-sources
winget install Microsoft.FoundryLocal    # one-time (local chat models)
cd onprem-rag-server && cargo run         # serves http://0.0.0.0:8000, GET /health
cd onprem-rag-app && pnpm install && pnpm tauri dev
```

Quick checks: `cargo build` (each crate) · `cargo test --bin onprem-server` · `npx tsc --noEmit` (app).
Note: `cargo run` can fail on hosts with Smart App Control enabled (os error 4551); check/build/test still work.

## Conventions

- Server routes return `Result<T, AppError>` (`error.rs` → JSON body + status). Bridge commands return
  `Result<T, String>`; keep the JWT in Rust managed state, **never** in the web layer.
- New server response fields must be mirrored in `commands.rs` + `bridge.ts`, or they're silently dropped.
- JSON-encode streamed SSE tokens — a bare `data:` strips leading spaces and fuses words on the client.
- Add heavy deps per workstream (not up front). Comments explain *why*; match surrounding naming/idiom.
