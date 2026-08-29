# onprem-rag-server

The Rust **[Rocket](https://rocket.rs/) 0.5** backend for the on-prem health-records RAG system.
It owns the source-DB connectors, Foundry Local (chat) + fastembed (embeddings), DocumentDB
storage and hybrid retrieval, auth, and the SSE streams the desktop app consumes. Everything runs
**on-prem** — no cloud model calls — because it handles PHI.

See the repo root [`CLAUDE.md`](../CLAUDE.md) for the full architecture and
[`plans/`](../plans/) for the design notes.

## Prerequisites

- **Rust** (stable, 2021 edition) — [https://rustup.rs](https://rustup.rs)
- **Docker** — runs DocumentDB (and, optionally, a seeded Postgres source). See
  [`docker-compose.yml`](../docker-compose.yml) at the repo root.
- **[Foundry Local](https://github.com/microsoft/Foundry-Local)** — the local chat LLM engine.
  One-time install on Windows: `winget install Microsoft.FoundryLocal`. Optional at boot: if it
  isn't running, the server still starts and the chat/model routes return `503` until it's
  available.
- Embeddings need **no** extra install — the [`fastembed`](https://github.com/Anush008/fastembed-rs)
  crate downloads the BGE-M3 model on first use into `.fastembed_cache/` (~2.3 GB; the first
  ingest is slow while this downloads — watch the app's log window or the server terminal).

## 1. Configure

Config comes from the environment (prefix `ONPREM_`). In dev it's read from the repo-root `.env`
via `dotenvy`. From the repo root:

```bash
cp .env.example .env
```

Then edit `.env` — at minimum set a real `ONPREM_CREDENTIALS_KEY` (used to encrypt stored
source-DB passwords at rest):

```bash
openssl rand -base64 32   # paste into ONPREM_CREDENTIALS_KEY=
```

`ONPREM_JWT_SECRET` should also be replaced with a long random string. The default admin
(`admin` / `password`, from `ONPREM_ADMIN_USERNAME`/`ONPREM_ADMIN_PASSWORD`) is seeded into
DocumentDB on first boot. See [`.env.example`](../.env.example) for every setting.

## 2. Start DocumentDB

DocumentDB is **required** — the server stores records + vectors there. From the repo root:

```bash
docker compose up -d documentdb
# Wait until it accepts connections:
docker compose logs -f documentdb   # look for "=== DocumentDB is ready ==="
```

Optional — a seeded Postgres "source" database with fake health records, so you can exercise
ingestion end-to-end without a real hospital system:

```bash
docker compose --profile dev-sources up -d
```

That creates `health_records` with a `patient_encounters` table (user/password `health`/`health`
on `localhost:5432` by default).

## 3. Run the server

```bash
cd onprem-rag-server
cargo run
```

It binds `http://0.0.0.0:8000` (override with `ONPREM_BIND_ADDRESS` / `ONPREM_PORT`). Confirm it's
up:

```bash
curl http://localhost:8000/health
```

`documentdb` in the response flips to `"down"` if the DB ping fails — the server still serves
`/health` so the app can report a degraded state.

Adjust log verbosity with `RUST_LOG` (default `info,onprem_server=debug`). These same events feed
the app's live log windows via `GET /logs/stream`.

## API

All routes except `POST /auth/login` and `GET /health` require `Authorization: Bearer <jwt>`.
The **Auth** column reads: _public_ (no token), _any_ (any authenticated user), or _admin_
(the caller's JWT role must be `admin`). Routes marked **SSE** stream `text/event-stream`.
[`server.http`](../server.http) (VS Code **REST Client** extension) has a ready-to-run request for
every endpoint — run **POST /auth/login** first and the rest reuse its token automatically.

### Health / Stats

| Method | Path           | Auth   | Notes                                                    |
| ------ | -------------- | ------ | -------------------------------------------------------- |
| GET    | `/health`      | public | Liveness + DocumentDB reachability (`up`/`down`).        |
| GET    | `/stats`       | any    | Dashboard summary (records, tables, connections, LLM).   |
| GET    | `/logs/stream` | any    | **SSE** live server `tracing` feed for the log windows.  |

### Auth + Users

| Method | Path                          | Auth   | Notes                                             |
| ------ | ----------------------------- | ------ | ------------------------------------------------- |
| POST   | `/auth/login`                 | public | Exchange username/email + password for a JWT.     |
| GET    | `/auth/me`                    | any    | The caller's own profile.                         |
| PATCH  | `/auth/me`                    | any    | Update own display name.                          |
| POST   | `/auth/me/password`           | any    | Change own password (requires current password). |
| GET    | `/auth/users`                 | admin  | List all users.                                   |
| POST   | `/auth/users`                 | admin  | Create a user.                                    |
| PATCH  | `/auth/users/<id>`            | admin  | Update a user's name / email / role.              |
| DELETE | `/auth/users/<id>`            | admin  | Delete a user.                                    |
| POST   | `/auth/users/<id>/password`   | admin  | Set a user's password (no current-password check).|

### Foundry / Models

| Method | Path                     | Auth  | Notes                                                        |
| ------ | ------------------------ | ----- | ------------------------------------------------------------ |
| GET    | `/hardware`              | any   | Execution providers + current chat model.                    |
| POST   | `/hardware/register-eps` | admin | Download + register all available EPs into the core.         |
| GET    | `/models`                | any   | Foundry Local catalog with cached/loaded state.              |
| POST   | `/models/select`         | admin | Download (if needed), load, and select a chat model.         |
| GET    | `/models/roles`          | any   | Per-role model manifest (chat, embeddings, reranker, …).     |
| POST   | `/models/pull`           | admin | **SSE** download (with progress) + optionally load a variant.|
| POST   | `/models/delete`         | admin | Unload + delete a variant's weights; clears its overrides.   |
| POST   | `/models/unload`         | admin | Unload a variant from memory (keeps weights on disk).        |
| PUT    | `/settings/router`       | admin | Persist / clear a role's model override.                     |
| POST   | `/generate`              | any   | **SSE** raw completion from the current chat model (smoke).  |
| GET    | `/setup-status`          | any   | One snapshot backing the Settings page (GPU, services, EPs). |

### Sources

| Method | Path                    | Auth  | Notes                                                     |
| ------ | ----------------------- | ----- | --------------------------------------------------------- |
| GET    | `/sources`              | any   | List saved sources (no secrets).                          |
| POST   | `/sources`              | admin | Test, then save a source (password encrypted at rest).    |
| POST   | `/sources/test`         | admin | Test an unsaved connection.                               |
| PATCH  | `/sources/<id>`         | admin | Edit a saved source.                                      |
| POST   | `/sources/<id>/test`    | admin | Re-test a saved source and persist the outcome.           |
| DELETE | `/sources/<id>`         | admin | Delete a source and cascade its ingested records.         |
| GET    | `/sources/<id>/schema`  | any   | Introspect the source's table + column schema.            |
| POST   | `/schema/analyze`       | any   | PII keyword pass (+ optional LLM enrichment); advisory.   |

### Ingest

| Method | Path                                    | Auth  | Notes                                              |
| ------ | --------------------------------------- | ----- | -------------------------------------------------- |
| POST   | `/ingest`                               | admin | Start an ingest job; returns a `job_id`.           |
| GET    | `/ingest/<job>/stream`                  | any   | **SSE** progress for a job (polls every 500 ms).   |
| GET    | `/ingest/history`                       | any   | Indexed-table history grouped by source.           |
| DELETE | `/ingest/table/<source_id>/<table>`     | admin | Remove one table's records + history entry.        |
| DELETE | `/ingest/connection/<source_id>`        | admin | Remove all records + history for one source.       |
| DELETE | `/ingest/all`                           | admin | Clear every record and indexed-table entry.        |

### Explorer / Audit

| Method | Path                       | Auth  | Notes                                                       |
| ------ | -------------------------- | ----- | ----------------------------------------------------------- |
| GET    | `/records`                 | any   | Row-grained, paginated, searchable browse of records.       |
| GET    | `/tables/<table_id>/info`  | any   | Per-table inspector (schema profile, recent runs).          |
| GET    | `/audit`                   | admin | Paginated, filterable, newest-first audit-log browse.       |

### Retrieval / Chat

| Method | Path      | Auth | Notes                                                          |
| ------ | --------- | ---- | -------------------------------------------------------------- |
| POST   | `/search` | any  | Run the retrieval pipeline; return ranked passages (no gen).   |
| POST   | `/chat`   | any  | **SSE** grounded, streamed answer with citations.              |

### Conversations

| Method | Path                            | Auth | Notes                                                   |
| ------ | ------------------------------- | ---- | ------------------------------------------------------- |
| GET    | `/conversations`                | any  | List the caller's plain-chat conversations.             |
| POST   | `/conversations`                | any  | Create a conversation.                                  |
| PATCH  | `/conversations/<id>`           | any  | Rename a conversation.                                  |
| DELETE | `/conversations/<id>`           | any  | Delete a conversation and cascade its messages.         |
| GET    | `/conversations/<id>/messages`  | any  | List a conversation's messages (oldest first).          |
| GET    | `/agent-conversations?<kind>`   | any  | List the caller's agent conversations of one kind.      |

### Agents

| Method | Path             | Auth | Notes                                                             |
| ------ | ---------------- | ---- | ----------------------------------------------------------------- |
| POST   | `/agents/<kind>` | any  | **SSE** task-aware agent (`auto`, `health_query`, `trends`, `patient_lookup`, `summarize`, `chat`). |

## Deployment: CORS & TLS

**CORS** — there is no CORS fairing, and that's correct. The desktop/Android client reaches the
server through the Tauri bridge's native `reqwest` HTTP client, not a browser `fetch`, so requests
carry no `Origin` header and trigger no preflight. A CORS fairing would only be needed if a real
browser-based SPA on a different origin were pointed at this server.

**TLS** — the server speaks plain HTTP on `0.0.0.0:8000`. On a trusted LAN (the intended on-prem
topology) that is acceptable; for anything beyond it, terminate TLS at a reverse proxy (nginx or
Caddy) in front of the server rather than in Rocket. Note that the server's own connection to
DocumentDB is already TLS (self-signed, `tlsAllowInvalidCertificates=true`).

## Quick checks

```bash
cargo check                  # fast type/borrow check (the gate used during development)
cargo build                  # full debug build
cargo build --release        # optimized build
cargo test                   # tests, where present (e.g. the router unit tests)
cargo fmt                    # format (cargo fmt --check to verify only)
cargo clippy --all-targets   # lints
```

Run an optimized server (much faster inference/ingest than the debug build):

```bash
cargo run --release
```

## Desktop app

The [`onprem-rag-app`](../onprem-rag-app/) Tauri client is the intended UI. Start this server
first, then follow that project's README.
