# On-premises RAG for health records

An on-premises Retrieval-Augmented Generation system for health records where **PHI never leaves the
premises - no cloud model calls**. Records living in operational SQL databases (PostgreSQL, MySQL, SQL
Server) are ingested through an on-prem server that embeds them with a **local** model, stores them
alongside their vectors in DocumentDB, and lets users **chat** with that data through a **local** LLM.
Embeddings, retrieval, reranking, and generation all run on-prem.

**All design and behavior documentation lives in [`docs/`](./docs/README.md).** This README is only an
entry point: what the system is, how to run it, and where the authoritative detail is.

## Features

| Feature | What it does | Details |
| --- | --- | --- |
| Grounded chat | Hybrid retrieval (vector + full-text) with cross-encoder reranking and inline citations; a score gate refuses to answer when nothing relevant is found. | [RAG patterns](./docs/rag-patterns.md), [Retrieval, chat memory, and concurrency](./docs/retrieval-chat-memory-and-concurrency.md) |
| Analytical agents | Structured questions (counts, breakdowns, trends over time) are routed to a validated DocumentDB aggregation pipeline or live-source SQL, then narrated and charted. | [Routing, agents, and structured query](./docs/routing-agents-and-structured-query.md), [Agentic patterns](./docs/agentic-patterns.md) |
| Tiered intent router | Separates conversational, structured, and semantic queries so each takes the cheapest correct path. | [Routing, agents, and structured query](./docs/routing-agents-and-structured-query.md), [Deterministic query handling](./docs/deterministic-sql-matcher.md) |
| Ingestion wizard | Pick a connection, select tables, then chunk, embed, and index with live progress and resumable checkpoints. | [Ingestion, schema catalog, and Data Explorer](./docs/ingestion-schema-catalog-and-explorer.md), [Data architecture](./docs/data-architecture.md) |
| Data explorer | Browse source connections, tables, and rows. | [Ingestion, schema catalog, and Data Explorer](./docs/ingestion-schema-catalog-and-explorer.md) |
| Local models and hardware | Foundry Local chat with GPU/NPU/CPU/OpenVINO detection, per-role model selection, and residency limits. | [Models, accelerators, and lifecycle](./docs/models-accelerators-and-lifecycle.md), [Role of Foundry Local](./docs/foundry-local-role.md), [Core LLM requirements](./docs/core-llm-requirements.md) |
| Auth and admin | JWT login, role-based access, user management, and an audit log. | [Security, authentication, and audit](./docs/security-auth-and-audit.md) |

## Components

- **[`onprem-rag-app/`](./onprem-rag-app/README.md)** - Tauri v2 desktop + Android client. A React 19
  frontend that renders the UI and **never holds the JWT**, over a Rust bridge that holds the JWT and
  makes the actual HTTP calls. Product surface and state ownership:
  [Application and feature architecture](./docs/application-and-feature-architecture.md).
- **[`onprem-rag-server/`](./onprem-rag-server/README.md)** - Rust **Rocket 0.5** backend: source-DB
  connectors, Foundry Local (chat), fastembed (embeddings + rerank), DocumentDB storage + hybrid
  retrieval, intent routing, and JWT auth. Runtime components and request paths:
  [System architecture design](./docs/system-architecture-design.md).

## Traffic flow

React -> Tauri `invoke` -> src-tauri bridge (holds the JWT, uses `reqwest`) -> server REST -> DocumentDB /
Foundry Local / source DBs. Streaming: server SSE -> bridge -> Tauri events.

Trust boundaries, concurrency, and failure behavior are specified in
[System architecture design](./docs/system-architecture-design.md) and
[Architecture and data model](./docs/architecture-and-data-model.md).

## Quickstart

```bash
# 0. Copy the config template and adjust.
cp .env.example .env

# 1. DocumentDB (required). Wait until it reports ready.
docker compose up -d documentdb
# Optional seeded Postgres source for end-to-end testing:
docker compose --profile dev-sources up -d

# 2. Foundry Local (one-time, Windows) - local chat models.
winget install Microsoft.FoundryLocal

# 3. Server - serves http://0.0.0.0:8000 (GET /health for liveness).
cd onprem-rag-server && cargo run

# 4. Desktop app.
cd onprem-rag-app && pnpm install && pnpm tauri dev
```

Configuration keys are documented in [`.env.example`](./.env.example); real secrets belong only in
ignored local environment files. Android builds and the REST surface are covered by the two component
READMEs, and [`server.http`](./server.http) holds ready-to-send requests.

## Documentation

[`docs/README.md`](./docs/README.md) is the authoritative index: it defines the status labels
(**Implemented** / **Partial** / **Planned**), the maintenance rules, the reading paths by role, and the
provenance matrix that maps every archived plan onto a current guide. Start there, or go straight to a
guide below.

### Core architecture and AI patterns

| Guide | Scope |
| --- | --- |
| [System architecture design](./docs/system-architecture-design.md) | Product clients, deployment topology, trust boundaries, runtime components, request paths, streaming, concurrency, failure behavior, delivery status. |
| [Data architecture](./docs/data-architecture.md) | Data ownership, lineage, provenance, source-row-to-chunk identity, ingest and catalog generations, last-known-good publication, retention and deletion. |
| [RAG patterns](./docs/rag-patterns.md) | Offline ingestion, hybrid vector/lexical retrieval, rewrite and expansion, RRF, deduplication, reranking, grounding, citations, refusal, verification, structured RAG. |
| [Agentic patterns](./docs/agentic-patterns.md) | Deterministic-first routing, task-aware model roles, typed and validated plans, guarded execution, fixed fallback graphs, run-scoped streaming, explicit limits on autonomy. |
| [Role of Foundry Local](./docs/foundry-local-role.md) | Foundry Local as the local generative-model runtime and lifecycle manager; excludes cloud Foundry, embedding, and reranking. |
| [DocumentDB architecture](./docs/documentdb-architecture.md) | The Internal DocumentDB Hybrid Store for documents, vectors, retrieval text, metadata, generations, conversations, and application state - distinct from external clinical sources. |

### Subsystem guides

| Guide | Scope |
| --- | --- |
| [Architecture and data model](./docs/architecture-and-data-model.md) | Trust boundaries, runtime components, request paths, DocumentDB collections, identifiers, generation semantics. Best starting point for the model. |
| [Application and feature architecture](./docs/application-and-feature-architecture.md) | Desktop App and Mobile App surface, shared Tauri/web-view implementation, state and event ownership, responsive shell, permissions. |
| [Ingestion, schema catalog, and Data Explorer](./docs/ingestion-schema-catalog-and-explorer.md) | SQL connectors, paged ingestion, checkpoints, last-known-good generations, schema metadata, drift, record browsing. |
| [Retrieval, chat memory, and concurrency](./docs/retrieval-chat-memory-and-concurrency.md) | Semantic RAG, hybrid retrieval, RRF, reranking, grounding, persisted memory, concurrent runs, queues, cancellation. |
| [Routing, agents, and structured query](./docs/routing-agents-and-structured-query.md) | Tiered intent routing, live-source SQL, DocumentDB aggregation, agent behavior, safety controls, fallback order, plus the planned router v3 and `StructuredExecutor` ladder. |
| [Deterministic query handling](./docs/deterministic-sql-matcher.md) | The planned `QuerySpec` IR (grammar, binder, per-dialect compiler, golden suite) and today's template matcher inventory, attempt order, refusal rules, fixtures. |
| [Hospital agents and their data map](./docs/hospital-agents-and-data-map.md) | **Planned.** Service-line ontology and per-source `SchemaBinding`: 13 service lines plus Ask, the entity concepts each owns, and the hooks that enforce the read allow-list. |
| [Models, accelerators, and lifecycle](./docs/models-accelerators-and-lifecycle.md) | Foundry Local and fastembed boundaries, task roles, execution providers, model residency, downloads, warmup. |
| [Core LLM requirements](./docs/core-llm-requirements.md) | Selection criteria for the shared Core LLM (tool calling, context window, local availability), the current allowlist, and how to add a model. |
| [Security, authentication, and audit](./docs/security-auth-and-audit.md) | JWT/RBAC, revocation, secrets, credential encryption, admission controls, audit coverage, risks, deployment requirements. |
| [Operations, performance, and observability](./docs/operations-performance-and-observability.md) | Readiness, limits, caching, instrumentation, metrics, live logs, performance constraints, validation workflow. |
| [Evaluation, production validation, and release](./docs/evaluation-production-and-release.md) | Automated and manual evidence, CI scope, production gates, versioning, updater state, required release artifacts. |

### Verified runtime facts and incidents

| Note | Scope |
| --- | --- |
| [Verified platform notes](./docs/verified-platform-notes.md) | Narrow, dated runtime facts proven against explicit environments, plus the conditions that invalidate them. |
| [A ~20 KB prompt silently kills the process hosting a Foundry Local model](./docs/foundry-local-webgpu-concurrency-crash.md) | Root cause, measured threshold, ruled-out hypotheses, and mitigations for the WebGPU execution-provider crash. |
| [Migration: chat generation to the Foundry Local daemon](./docs/foundry-service-migration.md) | Why generation moved out of the API process, what it does and does not fix, and how the blast radius changed. |

### Plans and history

- [`plans/new/00-README.md`](./plans/new/00-README.md) - the accepted target design that is not yet in
  code (service-line ontology, `SchemaBinding`, router v3, `QuerySpec` IR, single structured executor,
  `ConversationFocus`, app restructure), with build order and shared gates. A plan is a specification,
  not evidence of completion.
- [`plans/old/`](./plans/old/) - archived design and decision records. Useful for rationale, stale on
  status. When a historical plan conflicts with a guide in [`docs/`](./docs/README.md), the guide wins,
  and code and tests win over both.

### Evidence and tooling

- [`eval/`](./eval/README.md) - committed fixtures, the local evaluator (`run.mjs`), question sets, and
  dated benchmark notes. Gates and interpretation:
  [Evaluation, production validation, and release](./docs/evaluation-production-and-release.md).
- [`perf/`](./perf/) - load tooling (`load.mjs`). A script is not a completed capacity result.
- [`docker/`](./docker/) - container assets, including the seeded `dev-postgres` source.
- [`CLAUDE.md`](./CLAUDE.md) - contributor guidance and conventions for working in this repository.

## Repository map

| Path | Contents |
| --- | --- |
| [`onprem-rag-server/`](./onprem-rag-server/README.md) | Rust Rocket server; the authoritative runtime implementation. |
| [`onprem-rag-app/`](./onprem-rag-app/README.md) | React client and Tauri bridge (desktop + Android). |
| [`docs/`](./docs/README.md) | Authoritative documentation for current system behavior. |
| [`plans/`](./plans/new/00-README.md) | Accepted target design (`new/`) and archived history (`old/`). |
| [`eval/`](./eval/README.md) | Fixtures, evaluator, question sets, dated reports. |
| [`perf/`](./perf/) | Load-generation tooling. |
| [`docker/`](./docker/), [`docker-compose.yml`](./docker-compose.yml) | DocumentDB and optional seeded source containers. |
| [`.env.example`](./.env.example) | Every supported configuration key. |
| [`server.http`](./server.http) | Ready-to-send REST requests against a running server. |
