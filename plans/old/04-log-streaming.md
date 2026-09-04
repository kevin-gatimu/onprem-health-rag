# 04 — Live Log Windows (server tracing → app)

> Design note for the in-app log windows added so users can watch external-service activity
> (Foundry model download/load, fastembed model pull, DocumentDB index creation, ingestion,
> retrieval) without the server terminal. Implemented 2026-08-23.

## Problem

Long external operations gave the app zero feedback. The worst case observed: fastembed pulling
BGE-M3 (~2.3 GB ONNX) took ~20 min with a silent UI — indistinguishable from a hang. The only
signal was the server's terminal `tracing` output, invisible to the desktop client.

## Decisions

- **Source = server `tracing` only**, streamed over SSE. One broadcast layer captures *every*
  event, so no per-callsite instrumentation is needed and all existing `tracing::info!/warn!/…`
  calls light up the panels for free. (Byte-level download % lives on Foundry/fastembed's own
  stderr, not in `tracing` — out of scope.)
- **UI = per-page filtered panels**, not one global console. Filtering is by event `target` (the
  Rust module path), so a panel asks for categories and the store prefix-matches targets.

## Server (`onprem-rag-server/src/`)

- **`logstream.rs`** — the process-global hub.
  - `LogLine { seq: u64, ts, level, target, message }` (`Serialize`, `Clone`). `seq` from a
    static `AtomicU64` for client-side dedup/ordering; `ts` via `chrono::Utc::now()` (existing
    dep — avoids a subscriber time feature).
  - `LogHub { tx: broadcast::Sender<LogLine>, ring: Mutex<VecDeque<LogLine>> }`. `RING_CAP=300`,
    `CHANNEL_CAP=512`. `push` locks the ring (push + truncate) then `tx.send` (ignoring the
    "no receivers" error). `subscribe() -> (Vec<LogLine> snapshot, Receiver)` is taken **under
    the ring lock** so there's no gap between snapshot and live tail (a rare duplicate is
    tolerated and deduped client-side by `seq`).
  - `hub() -> &'static LogHub` via `OnceLock`.
  - `BroadcastLayer` implements `tracing_subscriber::Layer::on_event`: reads level/target from
    metadata, a `MessageVisitor` (`record_debug` fallback) captures the `message` field plus any
    `key={:?}` fields into one string, then `hub().push(line)`.
- **`main.rs`** — subscriber is now a `Registry` composed of the **same** `EnvFilter`, the
  existing `fmt::layer()` (terminal), and `BroadcastLayer` (app). `RUST_LOG` therefore controls
  what the app sees too. Default filter `info,onprem_server=debug`.
- **`routes/logs.rs`** — `GET /logs/stream`, `AuthUser`-guarded (any authenticated user).
  `EventStream![]`: yields the snapshot as `log` events, then loops `rx.recv()`; on
  `RecvError::Lagged(n)` yields a `warn` event ("dropped N log lines") and continues; on `Closed`
  breaks. Requires `tracing-subscriber` features `registry` + `fmt` (`env-filter` already present).

## Bridge (`onprem-rag-app/src-tauri/src/`)

- **`state.rs`** — `Bridge.log_streaming: AtomicBool` guards against starting more than one relay
  per session (many panels share the one stream).
- **`commands.rs`** — `start_log_stream(app, bridge)`: `swap(true)` on the guard → early-return if
  already live; else GET `/logs/stream` with `bearer_auth`, relay each `log` event's data to the
  Tauri event `logs://line` (payload = the raw `LogLine` JSON string), `warn`/errors to
  `logs://error`. The guard is cleared when the stream ends (e.g. logout → 401), so a re-login
  can restart it.
- **`lib.rs`** — command registered in `invoke_handler`.

## Frontend (`onprem-rag-app/src/`)

- **`lib/bridge.ts`** — `LogLine` interface + `startLogStream()`.
- **`logs/logStore.ts`** — module-level shared buffer so all panels share one subscription:
  first `useLogs` mount calls `startLogStream()` (idempotent) and registers one `logs://line`
  listener; lines are parsed, deduped by `seq`, kept in a `RING_CAP=1000` ring. `useLogs(categories)`
  returns the category-filtered view via `useSyncExternalStore`. **Gotcha:** `getSnapshot` must
  return a *stable* reference when nothing changed, or `useSyncExternalStore` render-loops — so
  the filtered array is cached against the buffer identity + category key and only recomputed when
  either changes; the empty case returns a shared `EMPTY` constant. `CATEGORY_TARGETS` maps each
  category to Rust module prefixes (`foundry`→`onprem_server::foundry`, `embed`→`::embed`,
  `ingest`→`::ingest`, `connectors`→`::connectors`, `documentdb`→`::documentdb`,
  `retrieval`→`::retrieval`/`::rag`).
- **`logs/LogConsole.tsx`** — reusable panel `{ title, categories, height? }`: monospace,
  level-colored, auto-scrolling; **pause** (freezes the view, buffers continue), **clear** (clears
  the shared buffer), and a **min-level** dropdown.
- Embedded: `settings/Settings.tsx` → `categories={["foundry"]}` ("Model & hardware activity");
  `sources/Sources.tsx` → `categories={["ingest","connectors","documentdb","embed"]}`
  ("Ingestion activity").

## Verified

`cargo check` clean in both Rust crates (only the 3 pre-existing benign dead-code warnings);
`npx tsc --noEmit` clean. `server.http` has a `GET /logs/stream` request for a manual 401/feed
check.

## Possible follow-ups

- Chat page panel (`categories={["retrieval","embed"]}`) — trivial, deferred.
- If byte-level download progress is ever wanted, it must be sourced from Foundry/fastembed
  directly (their stderr / SDK callbacks), not from `tracing`.
