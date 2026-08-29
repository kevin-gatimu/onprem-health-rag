# On-Prem Health-Records RAG — Master Plan

> Persistent design "mind cache." This is the approved implementation plan. Add numbered design notes
> alongside it (`01-...md`, `02-...md`) as decisions are made during the build.

## Context

We are building an **on-premises RAG system for health records**. Health data lives in operational SQL databases (PostgreSQL, MySQL, SQL Server). We (1) ingest those records through an on-prem server that calls a **local** embedding model and stores both the raw records and their vector embeddings in a document database, then (2) let users **chat with that data** through a local chat LLM. Everything runs on-prem — no cloud model calls — which matters for PHI/privacy.

- **`onprem-rag-app/`** — Tauri v2 desktop app. React 19 + Vite 7 frontend renders UI and manages state; `src-tauri/` (Rust) is a **bridge** that calls the server's REST endpoints and relays streams to the frontend as Tauri events.
- **`onprem-rag-server/`** — Rust (Rocket 0.5) server. Source-DB connectors, Foundry Local (chat + embeddings), hardware/EP detection, DocumentDB storage + hybrid search, auth. Network-exposed; handles many concurrent desktop clients.

**Topology:** one server host runs the Rocket server + **Foundry Local** + **DocumentDB (Docker)**. Many desktop apps connect over the network. "Distributed" = async Rocket (Tokio) + connection pooling + stateless JWT.

**Auth:** default `admin` / `password` seeded into DocumentDB (argon2-hashed). Enforced at three layers: DocumentDB connection credentials, server-side JWT + Rocket request guards, and a UI login gate.

## Confirmed technical facts (research, Aug 2026)

**Foundry Local** (Microsoft, preview): crate `foundry-local-sdk` (native in-process ONNX Runtime; `--features winml` on Windows). `FoundryLocalManager`, `catalog().get_model(alias)`, `model.download()/load()`, `create_chat_client()`, embedded OpenAI server (`start_web_service()` + `manager.urls()`, dynamic port). Chat model chosen: **`qwen2.5-7b-instruct-openvino-gpu:2`** (was `phi-4-mini`). EPs auto-selected (CUDA/TensorRT, OpenVINO, QNN, VitisAI, WebGPU, CPU); user override by loading a specific variant id (`-generic-cpu`, `-cuda-gpu`); enumerate via `foundry model list --device {cpu|gpu|npu} --variants`. Install `winget install Microsoft.FoundryLocal`. Over REST use resolved `model.id()`.

**⚠️ CORRECTION (verified 2026-08-23 against installed Foundry Local 0.8.119):** the catalog contains **NO embedding models** — all 36 aliases are chat/reasoning/vision (`foundry model run` reports "Chat completion support" only; there is no `--type embedding` filter and `qwen3-embedding-0.6b` is not present). Host EPs on this machine: `CPUExecutionProvider`, `WebGpuExecutionProvider`, `OpenVINOExecutionProvider` (no NVIDIA/CUDA). **Consequence:** Foundry Local is used for **chat only**. Embeddings are produced **locally via the `fastembed` crate** (same crate already used for reranking — ONNX Runtime under the hood, fully on-prem/air-gappable). Exact embedding model + dims finalized below once fastembed capability research completes; `create_embedding_client()` is dropped from the design.

**DocumentDB** (documentdb.io / Azure DocumentDB engine, MongoDB-wire, pgvector): image `ghcr.io/documentdb/documentdb/documentdb-local:latest`, port `10260`, creds via `--username/--password`, SCRAM-SHA-256, self-signed TLS. Conn string `mongodb://<u>:<p>@<host>:10260/?tls=true&tlsAllowInvalidCertificates=true&retrywrites=false`. Wait for `=== DocumentDB is ready ===`. Rust `mongodb` 3.x. Vector search: `createIndexes` with `key:{field:"cosmosSearch"}`, `cosmosSearchOptions{ kind, dimensions:1024, similarity:"COS" }`; query via `$search`/`cosmosSearch` stage.

**Hybrid + rerank:** full-text via GA legacy `$text` index (`$meta:"textScore"`) — on-prem-safe default; BM25 `$search` engine is Gated Preview (upgrade only). No native `$rankFusion` on this engine — run vector + full-text as two aggregations, fuse with **RRF** `Σ 1/(k+rank)`, `k=60`, in Rust. Rerank not in Foundry/DocumentDB — use `fastembed` 6.x `TextRerank` cross-encoder (`bge-reranker-v2-m3`) on top-N≈30 → top-k≈6. Air-gapped: `try_new_from_user_defined` with pre-staged ONNX+tokenizer.

**Connectors:** `sqlx` 0.8 (`runtime-tokio`+`postgres`+`mysql`+`tls-rustls`) for PG/MySQL; `tiberius` (+`tokio-util` compat) for MSSQL (sqlx dropped MSSQL).

## Structure

```
RAG/
  docker-compose.yml    # DocumentDB (required) + optional dev-sources profile
  CLAUDE.md
  .env.example
  plans/                # this folder
  onprem-rag-server/    # Rocket server (module-organized)
  onprem-rag-app/       # Tauri v2 app (React + src-tauri bridge)
```

### Server modules — `onprem-rag-server/src/`
- `main.rs` — Rocket launch, mount routes, `.manage(AppState)`, CORS fairing, bind 0.0.0.0
- `config.rs` — env config; `state.rs` — AppState; `error.rs` — AppError→Responder
- `auth/` — jwt, password (argon2), guard (AuthUser), seed (admin/password), routes
- `documentdb/` — connect + typed collections; vector.rs (cosmosSearch + `$text` indexes, knn + full-text helpers)
- `foundry/` — native in-process `FoundryLocalManager` singleton initialization (no external process/port); hardware.rs (EPs/devices/variants); chat.rs (stream); routes.rs (setup-status, model-unload)
- `retrieval/` — mod (vector+full-text→RRF→optional rerank); rrf.rs; rerank.rs (fastembed)
- `connectors/` — SourceConnector trait + factory; postgres.rs, mysql.rs (sqlx); mssql.rs (tiberius)
- `ingest/` — pipeline (fetch→text→embed→upsert) + job progress; routes (sources, ingest, SSE)
- `rag/` — retrieve→prompt; routes (`POST /chat` SSE, `POST /search` debug)
- `routes/health.rs` — `GET /health`

**Server deps:** rocket(0.5,json), tokio, mongodb(3), foundry-local-sdk(+winml on win), sqlx(0.8), tiberius+tokio-util, serde/serde_json, jsonwebtoken, argon2, reqwest(0.12,json+stream), fastembed(6.x), async-trait, uuid, chrono, futures, thiserror, tracing/tracing-subscriber, rocket_cors (or custom fairing).

### Collections
- `users` `{ _id, username, password_hash, role, created_at }` — seed admin/password (argon2)
- `sources` `{ _id, name, kind, host, port, database, username, password_enc, query|tables, created_by }` — creds encrypted at rest
- `records` `{ _id, source_id, row_pk, fields{}, text, contentVector:[f32;1024], metadata, ingested_at }` — cosmosSearch vector index + `$text` index on `text`
- `jobs` `{ _id, source_id, status, total, processed, errors[], started_at, finished_at }`

### REST API (all except /auth/login and /health require Bearer JWT)
`POST /auth/login` · `GET /auth/me` · `POST /auth/users` · `GET /health` · `GET /hardware` · `GET /models` + `POST /models/select` · `GET /sources` · `POST /sources` · `POST /sources/test` · `POST /ingest` · `GET /ingest/<job>/stream` (SSE) · `POST /chat` (SSE; opts: mode `vector|hybrid`, `top_k`, `rerank`) · `POST /search` (debug).

### Tauri bridge — `onprem-rag-app/src-tauri/`
Deps: reqwest(json+stream), tokio, futures-util, eventsource-stream. Managed AppState = server base URL + in-memory JWT (Rust holds token). Commands mirror API: login/me/create_user/get_hardware/list_models/select_model/list_sources/save_source/test_source/start_ingest/chat. Streaming: consume server SSE, re-emit as Tauri events (`chat://token`, `chat://done`, `ingest://progress`). Enable `core:event:default` capability.

### React — `onprem-rag-app/src/`
Deps: react-router-dom, @tanstack/react-query, zustand, Tailwind. Session store + `RequireAuth`. Screens: Login → Settings/Hardware → Sources → Chat.

### docker-compose.yml
`documentdb` service (image/port/creds/volume/healthcheck). Optional `dev-sources` profile: seeded Postgres with sample health rows.

## Build order (all delivered)
1. **Foundations** ✅ DONE — compose + .env.example; server deps/config/state/error/main+`/health`; DocumentDB connect; Tauri bridge skeleton. Gate MET: `/health` round-trips through the bridge; DocumentDB reports "up".
2. **Auth** ✅ DONE — argon2id, admin seed on boot, JWT HS256, AuthUser guard, `/auth/login|me|users`; bridge login/me/logout (JWT in Rust state); React SessionProvider + Login gate. Gate MET (verified vs live DocumentDB): admin/password login returns JWT; `/auth/me` 401s without token; non-admin create 403; duplicate 400.
3. **Foundry + hardware** ✅ CODE-COMPLETE — server `foundry/{mod,routes}.rs` (native in-process `FoundryLocalManager`, `&'static` singleton in `AppState`): `GET /hardware` (EPs via `discover_eps`, deduped), `GET /models` (catalog + cached/loaded), `POST /models/select` (admin: resolve→download→load), `POST /generate` (SSE test). Bridge cmds `get_hardware`/`list_models`/`select_model`/`generate` (SSE→`chat://token|error|done` Tauri events). React `Settings.tsx` tab: EP chips, model table, streaming test prompt. All three layers build clean (cargo check ×2, tsc). Gate: ⏳ needs live run w/ Foundry Local installed to confirm EPs list + model loads + prompt streams. **Chat = only (no embeddings in Foundry catalog); embeddings via fastembed BGE-M3 in WS5.**
4. **Connectors** ✅ CODE-COMPLETE — `connectors/`: `SourceConnector` trait (`test()`; `fetch` deferred to WS5) + `SourceSpec`/`SourceKind` + factory. PG/MySQL via **sqlx 0.8** (`PgConnectOptions`/`MySqlConnectOptions`, `connect_with`, 8s acquire timeout, `SELECT 1`); MSSQL via **tiberius 0.12** (rustls, no native-tls; `tokio_util::compat`; `trust_cert`; `simple_query`). Creds encrypted at rest: `crypto.rs` AES-256-GCM, key = SHA-256(domain ‖ `ONPREM_CREDENTIALS_KEY` else jwt_secret), stored `base64(nonce‖ct)`. Routes: `GET /sources` (any authed), `POST /sources/test` + `POST /sources` (admin; save tests first). Bridge `list_sources`/`test_source`/`save_source`; React `Sources.tsx` tab (list + add form w/ Test then Save). All three layers build clean. Gate: ⏳ needs Docker + live PG/MySQL/MSSQL to confirm test-connection (Docker Desktop not running at build time).
5. **Ingestion + indexes** ✅ CODE-COMPLETE — `embed/mod.rs`: fastembed **BGE-M3** (1024-dim, prefix-free) as a process-global `OnceLock<Mutex<TextEmbedding>>`, all embedding in `spawn_blocking` (fastembed is sync + `&mut self`); `embed_documents`/`embed_query` (seam kept for future asymmetric model); dim-mismatch guard. `documentdb/vector.rs`: `ensure_indexes` → `createIndexes` cosmosSearch (`vector-ivf`, numLists 100, COS, dims from config) + legacy `$text` index (⚠️ cosmosSearch syntax still VERIFY-EARLY vs live container). Connectors gained `fetch(limit)`: PG via `to_jsonb(_sub)` whole-row JSON; MySQL/MSSQL per-column try-fallthrough decode; `FetchedRow{pk,fields,text}` with pk-candidate detection + `key: value` text projection; `load_spec` decrypts saved source. `ingest/mod.rs`: pipeline ensure_indexes → load_spec → fetch → chunk (word-based ~384/64 approx) → embed in batches of 32 → delete prior records for source → insert `RecordDoc{_id=source:pk:chunk, …, contentVector}` → update job; runs as detached `tokio::spawn` (survives dropped connection), errors write a failed job. `ingest/routes.rs`: `POST /ingest` (admin: verify source, insert running job, spawn, return `job_id`) + `GET /ingest/<job>/stream` (SSE **polling** the job doc every 500ms → `progress`/`error` events, stops on terminal status). Bridge `start_ingest` (POST then follow stream → `ingest://progress|done|error` Tauri events); React `Sources.tsx` per-source Ingest button + live progress bar. All three layers build clean (cargo build ×2, tsc). Gate: ⏳ needs live DocumentDB + a source to confirm cosmosSearch index creation, 1024-dim vectors persisted, both indexes present.
6. **Hybrid retrieval + RAG chat** ✅ DONE (2026-08-25) — vector+`$text`→RRF(k=60)→optional fastembed rerank→prompt→stream; `/search` + `/chat` SSE; Chat UI + toggle. Gate: `/search` shows fused+reranked hits; cited streamed answer; toggling mode/rerank reorders.
7. **AI Agents** ✅ DONE (2026-08-25) — agent-partitioned conversations via `agent_kind` discriminator; auto routing (lexical → optional model → fail-open to Chat); structured aggregation (trends/health_query) vs semantic retrieval (lookup/summarize); SSE routed/spec/rows/pipeline/citations/token; client-derived activity strip; chart/citation persistence. Gate: all three layers build clean (cargo check + tsc --noEmit exit 0); agent conversations persist with structured/semantic results; reload confirms re-render from history.
8. **Models + Settings** ✅ DONE (2026-08-25) — System Setup + Models screens; `GET /setup-status` (degraded-safe); `POST /models/unload` (per-variant memory free); server-side unload/pull/select/delete admin-guarded; bridge mirrors all structs; Re-register EPs action (not start/stop/restart — Foundry is in-process native singleton, no external lifecycle); read-only fastembed info. Settings shows hardware + service health + active model selector + loaded-models unload-all; Models shows role sections with variant cards (download/load/unload/delete/set-default state machine). No Docker lifecycle control (DocumentDB read-only + guidance). Gate: ✅ all three layers build clean (cargo check + tsc exit 0); admin actions work; degraded path returns 200 when Foundry down.
9. **Profile + Admin** ✅ DONE (2026-08-25) — self-service Profile (all roles: name edit, change-password) + admin User Management (list/create/edit/delete/set-password); server `created_at` on `UserInfo` + 6 auth routes (`GET /auth/users`, `PATCH`/`DELETE /auth/users/<id>`, `POST /auth/users/<id>/password`, `PATCH /auth/me`, `POST /auth/me/password`) with last-admin + self-delete guards; audit write-side wired. Gate: all three layers build clean (cargo check ×2 + tsc --noEmit exit 0).
10. **Audit Log** ✅ DONE (2026-08-25) — admin-only `GET /audit?user&action&from&to&page` (`$facet` paginate, newest-first, `$toString` on ObjectId `_id`, `parse_bound` date helper accepting YYYY-MM-DD/RFC3339, username-substring + exact-action filters) + 5 completed write-path sites (user_created, source create/update/delete, ingest_started). Gate: cargo check + tsc --noEmit both exit 0.
11. **Analytics + Alerts stubs, polish, Android build** ✅ DONE (2026-08-25) — honest Analytics/Alerts StubPages + global ErrorBoundary; Android manifest cleartext forced on; server README API table (49 routes) + CORS/TLS deployment note, app README Android section, new root README. `tauri android init` was already run (gen/android committed). tsc exit 0. Android build/run + hardware-back button binding deferred to on-device work (no stable Tauri v2 JS API for hardware back; host build blocked by Smart App Control / OS error 4551).

## Defaults (adjustable)
- Embeddings `qwen3-embedding-0.6b` (1024 dims, asymmetric — query gets qwen3 instruction prefix, docs plain).
- Chat default `qwen2.5-7b-instruct-openvino-gpu:2` (7B, OpenVINO-GPU variant; swappable; chosen over phi-4-mini for grounding).
- Foundry: prefer native SDK clients; REST fallback; discover URL, never hardcode port.
- Auth: JWT HS256, argon2id, stateless; bridge holds token.
- Retrieval default: hybrid, ~top-50/side, RRF k=60, rerank ON (bge-reranker-v2-m3), top-N≈30→top-k≈6; all toggleable.
- Query expansion default ON: multi-query (3 variants) → retrieve each → RRF together.
- Chunking default ON: embed per passage, ~384-token windows / 64 overlap; structured fields kept as metadata.
- Records: generic — user-supplied table/query, row flattened to JSON + text projection.

## Verify-early flags
- ✅ RESOLVED (2026-08-23, see `02-verification-and-decode-findings.md`): `cosmosSearch` create-index +
  `$search`/`$text` behavior proven live against the local container; MSSQL DECIMAL/NUMERIC decode fixed
  (→ string) and MONEY (→ f64) understood. `vector.rs` unchanged.
- Exact `fastembed` 6.x `RerankerModel` variant names + `RerankResult` shape (`ort` rc API shifts) — WS6.

## Improvement roadmap (planned 2026-08-29)
- `17-intent-router-v2.md` — tiered router (conversational gate → lexical → phi-4-mini tool-call classify + cache); structured vs semantic vs hybrid; structured-backend selection (DocDb vs SourceSql).
- `18-text-to-sql-harness.md` — dialect-aware NL→SQL vs live PG/MySQL/MSSQL: `schema_catalog` + `sql_examples` in DocumentDB, schema linking, phi-4-mini generation, sqlparser-rs policy gate, read-only execution, repair pass; covers aggregation/distribution query classes.
- `19-retrieval-and-faithfulness.md` — calibrated score gate ON, metadata/patient filters, parent-row context expansion, citation discipline + NPU verifier pass, token-accurate chunking, ICD-code phrase handling.
- `20-performance-fast-path.md` — fold rewrite+expand into one call, parallel retrieval fan-out, embed LRU, boot warmup, rerank budget, HNSW option, SSE status pipelining, ingest pipelining.
- `21-eval-harness.md` — port RAGAs eval + judge-free hit-rate suite to this stack; per-stage tracing spans, `/metrics/summary`, search provenance. Semantic and structured paths instrumented separately.
- `22-chat-memory-and-compaction.md` — server-authoritative chat memory: rolling summary + token-budgeted verbatim tail, write-behind compaction, history in the generation prompt (not just rewrite), standalone question into the structured path, sticky focus entities, retention sweep. Client = render cache only (no PHI on disk).
