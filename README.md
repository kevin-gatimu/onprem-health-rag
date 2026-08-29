# On-premises RAG for health records

An on-premises Retrieval-Augmented Generation system for health records where **PHI never leaves
the premises — no cloud model calls**. Health data living in operational SQL databases (PostgreSQL,
MySQL, SQL Server) is ingested through an on-prem server that calls a **local** embedding model,
stored alongside its vector embeddings in DocumentDB, and then users **chat** with that data through
a **local** chat LLM. Everything — embeddings, retrieval, and generation — runs on-prem.

## Components

- **[`onprem-rag-app/`](./onprem-rag-app/README.md)** — the Tauri v2 desktop + Android client. A
  React 19 frontend that renders the UI and **never holds the JWT**, over a Rust bridge that holds
  the JWT and makes the actual HTTP calls to the server.
- **[`onprem-rag-server/`](./onprem-rag-server/README.md)** — the Rust **Rocket 0.5** backend: the
  source-DB connectors, Foundry Local (chat), fastembed (embeddings + rerank), DocumentDB storage +
  hybrid retrieval, and JWT auth.

## Traffic flow

React → Tauri `invoke` → src-tauri bridge (holds the JWT, uses `reqwest`) → server REST → DocumentDB /
Foundry Local / source DBs. Streaming: server SSE → bridge → Tauri events.

## Quickstart

```bash
# 1. DocumentDB (required). Wait for "=== DocumentDB is ready ===".
docker compose up -d documentdb
# Optional seeded Postgres source for end-to-end testing:
docker compose --profile dev-sources up -d

# 2. Foundry Local (one-time, Windows):
winget install Microsoft.FoundryLocal

# 3. Server — serves http://0.0.0.0:8000 (GET /health for liveness).
cd onprem-rag-server && cargo run

# 4. Desktop app.
cd onprem-rag-app && npm install && npm run tauri dev
```

For the full setup detail (configuration, prerequisites, the API, Android builds) see the two
component READMEs linked above.

## Documentation

- [`CLAUDE.md`](./CLAUDE.md) — repo guidance and the full architecture overview.
- [`plans/`](./plans/) — numbered design notes (the project's "mind cache"); start at
  [`plans/00-master-plan.md`](./plans/00-master-plan.md).
