# On-premises RAG for health records

An on-premises Retrieval-Augmented Generation system for health records where **PHI never leaves the
premises — no cloud model calls**. Records living in operational SQL databases (PostgreSQL, MySQL, SQL
Server) are ingested through an on-prem server that embeds them with a **local** model, stores them
alongside their vectors in DocumentDB, and lets users **chat** with that data through a **local** LLM.
Embeddings, retrieval, reranking, and generation all run on-prem.

## Features

- **Grounded chat** — hybrid retrieval (vector + full-text) with cross-encoder reranking and inline
  citations; a score gate refuses to answer when nothing relevant is found.
- **Analytical agents** — structured questions (counts, breakdowns, trends over time) are routed to a
  validated DocumentDB aggregation pipeline, then narrated and charted.
- **Tiered intent router** — separates conversational, structured, and semantic queries so each takes
  the cheapest correct path.
- **Ingestion wizard** — pick a connection, select tables, then chunk → embed → index with live progress.
- **Data explorer** — browse source connections, tables, and rows.
- **Local models & hardware** — Foundry Local chat with GPU/NPU/CPU/OpenVINO auto-detection and
  per-role model selection.
- **Auth & admin** — JWT login, role-based access, user management, and an audit log.

## Components

- **[`onprem-rag-app/`](./onprem-rag-app/README.md)** — Tauri v2 desktop + Android client. A React 19
  frontend that renders the UI and **never holds the JWT**, over a Rust bridge that holds the JWT and
  makes the actual HTTP calls.
- **[`onprem-rag-server/`](./onprem-rag-server/README.md)** — Rust **Rocket 0.5** backend: source-DB
  connectors, Foundry Local (chat), fastembed (embeddings + rerank), DocumentDB storage + hybrid
  retrieval, intent routing, and JWT auth.

## Traffic flow

React → Tauri `invoke` → src-tauri bridge (holds the JWT, uses `reqwest`) → server REST → DocumentDB /
Foundry Local / source DBs. Streaming: server SSE → bridge → Tauri events.

## Quickstart

```bash
# 0. Copy the config template and adjust.
cp .env.example .env

# 1. DocumentDB (required). Wait until it reports ready.
docker compose up -d documentdb
# Optional seeded Postgres source for end-to-end testing:
docker compose --profile dev-sources up -d

# 2. Foundry Local (one-time, Windows) — local chat models.
winget install Microsoft.FoundryLocal

# 3. Server — serves http://0.0.0.0:8000 (GET /health for liveness).
cd onprem-rag-server && cargo run

# 4. Desktop app.
cd onprem-rag-app && pnpm install && pnpm tauri dev
```

For the full setup (configuration, the REST API, Android builds) see the two component READMEs linked
above.

## Documentation

- [`CLAUDE.md`](./CLAUDE.md) — contributor guidance and the architecture overview.
- [`plans/docs/README.md`](./plans/docs/README.md) — authoritative current documentation and its
  maintenance policy. Historical plans are preserved in [`plans/old/`](./plans/old/).
