# Migration: chat generation from the in-process core to the Foundry Local daemon

**Why:** ONNX Runtime's WebGPU execution provider kills the process hosting the model when a
prompt exceeds ~20 KB — silently, uncatchably, on a thread the host never called into. In-process
that process is the API server; in the daemon it is a restartable child. Evidence, the measured
threshold and the ruled-out hypotheses are in
`plans/docs/foundry-local-webgpu-concurrency-crash.md`.

**Status: implemented and verified.**

**What this buys, precisely.** It does *not* stop the crash — the daemon dies on an oversized
prompt exactly as the in-process core does. It changes the blast radius: the API survives, returns
a `503`, follows the daemon to its new port on the next request, and the fault becomes visible in
the daemon's log instead of vanishing. Preventing the crash is the separate prompt cap
(`ONPREM_PROMPT_MAX_BYTES`).

**Shape of the change:** generation moves to HTTP against the `foundry` daemon. Everything else
about the pipeline — routing, retrieval, the planner, personas, SSE to the client — is untouched.

---

## What moves and what stays

| Concern | Before | After |
| --- | --- | --- |
| Chat generation (stream + non-stream) | SDK `ChatClient` → in-process core | **HTTP** to daemon `/v1/chat/completions` |
| Model load / unload / loaded-list | SDK → in-process core | Daemon `/models/load/{id}`, `/models/unload/{id}`, `/models/loaded` |
| Catalog, variant resolution, aliases | SDK `Catalog` | unchanged (SDK) |
| Hardware detection | `hardware.rs` (PowerShell + sysinfo) | unchanged |
| EP discovery + registration | in our process | **dropped** — the daemon owns its execution providers |
| Embeddings, reranking (fastembed) | in-process ONNX | unchanged — they never had the fault |

Dropping EP registration matters on its own: `download_and_register_eps` is what pulls
`onnxruntime_providers_webgpu.dll` into our address space in the first place.

## Design decisions

**Keep the SDK.** Catalog and variant resolution (alias → `qwen3-8b-generic-gpu:2`, device
class, context caps) are non-trivial and correct today. The model router depends on them. Only
the generation call changes.

**Reuse the wire types.** The daemon speaks OpenAI, and the SDK's `ChatCompletionStream` is
already `JsonStream<CreateChatCompletionStreamResponse>` over **`async-openai` types**, which we
already depend on. Deserialising the daemon's SSE into the same
`CreateChatCompletionStreamResponse` means `GuardedChatStream` keeps its `Stream::Item` type and
**every consumer stays unchanged** — `agents/routes.rs`, `answer/mod.rs`, `rag/routes.rs`,
`foundry/routes.rs`. Verified: the daemon emits `object: "chat.completion.chunk"` with
`choices[].delta.content`, plus extra fields serde ignores.

**Endpoint discovery, in order:** `ONPREM_FOUNDRY_SERVICE_URL` → `~/.foundry/daemon.json`
(`web_urls[0]`, which the daemon writes itself alongside `pid` and versions) → error. The port is
assigned per daemon start (observed moving 55862 → 58006), so it must never be hardcoded.

**Keep a working in-process fallback.** `ONPREM_FOUNDRY_BACKEND=service|in_process`, defaulting
to `service`. The in-process path stays compilable and selectable for a host with no daemon
installed, and it is the one that carries the crash — so the default matters and the fallback is
documented as unsafe under concurrency.

## Concurrency after the move

`ONPREM_MAX_ACTIVE_GENERATIONS` stays at `1`, but for a different reason than first thought. It is
not what prevents the crash — prompt size is. It is kept because the daemon executes serially
anyway (measured: 8 concurrent short prompts = 166 s ≈ 8 × 17 s), because a model host serving one
request at a time is easier to reason about, and because `unload`-then-`load` churn has
independently been observed to kill the daemon.

What changed is the *wait*. `ONPREM_GENERATION_QUEUE_TIMEOUT_MS` (default 300 s) applies to the
generation semaphore only, so a second asker queues and gets an answer instead of a `429` after
2 s. Retrieval and ingestion keep the short `ONPREM_ADMISSION_TIMEOUT_MS`, where fast rejection
really is the right response to overload.

The process-wide native-inference gate prototyped during diagnosis has been **removed**: it did
not prevent the crash, and it would have serialised ingest embedding behind chat for no benefit.

---

## Steps

### 1. Dependencies and config
- Add `reqwest` (rustls, `json`, `stream`) to the server crate. It is already present
  transitively via the SDK; make it direct since we now call HTTP ourselves.
- `config.rs`: add `foundry_backend` (`ONPREM_FOUNDRY_BACKEND`, default `service`) and
  `foundry_service_url` (`ONPREM_FOUNDRY_SERVICE_URL`, default none → discover).
- `.env.example`: document both, and the daemon prerequisite.

### 2. `src/foundry/service.rs` (new)
- `discover_endpoint()` — env, then `~/.foundry/daemon.json`.
- `ServiceClient { base, http }` with:
  - `chat_stream(model, msgs, tools, tool_choice, params)` → SSE → `CreateChatCompletionStreamResponse`
  - `chat_once(...)` → `CreateChatCompletionResponse`
  - `load(id)` / `unload(id)` / `loaded()`
  - `health()` for startup probing
- Map non-2xx and the daemon's `{"error":{...}}` body onto `AppError`, so a native fault at the
  daemon becomes a clean error instead of a dead server.

### 3. `src/foundry/mod.rs`
- `GuardedChatStream` wraps a boxed stream so either backend can produce it; item type unchanged.
- `generate_stream_with` / `complete_with` / `plan_tool` dispatch on the backend.
- `ensure_loaded_lru` routes load/unload through the daemon when on the service backend; the
  resident cap still applies (one GPU either way).
- Skip in-process EP registration on the service backend.

### 4. Startup
- Probe the daemon on boot; log its URL and version. Failure is non-fatal and degrades chat only,
  matching how a missing in-process core is already handled.

### 5. Verification — results

- `cargo test --bin onprem-server`: **483 passed**.
- Two concurrent agent requests (`/agents/pharmacy` and `/agents/ask`, the original repro):
  **both answered**, server alive, daemon alive. 645 s wall clock — they queue, they do not run
  in parallel.
- Server RSS **375 MB** with the model warm, against 8.2 GB (`qwen3-8b`) or 17.7 GB
  (`qwen3-14b`) when the model ran in-process.
- Forced tool calls work against the daemon, so the structured planner path survives.
- Daemon down: the server returns
  `503 Foundry daemon unreachable … Start it with 'foundry model load <model>'` and keeps serving
  every other route, instead of dying.

Known gap: with the prompt cap in force, the unscoped **Ask** agent's 63-table catalog is
truncated, and it answered "the data provided does not include a list of medicines" where the
department-scoped Pharmacy agent answered from the catalog. The cap trades answer quality for a
live process; scoping the planner catalog properly is the follow-up that removes the trade.

## Operating the daemon

`foundrylocald` is auto-started by any `foundry` CLI invocation, but a daemon started that way is
parented to that transient command and dies with it — observed repeatedly during this work. Start
it deliberately:

```bash
foundry server start        # survives; `foundry server status` shows uptime and PID
foundry model load qwen3-8b
```

Each start binds a **new port** and rewrites `~/.foundry/daemon.json`. Nothing may hardcode the
port: the server reads that file at boot and re-reads it whenever a request cannot connect.

Daemon and server must also agree on the model cache — the daemon uses `~/.foundry/config.json`
(`cache_directory`, set with `foundry cache cd <path>`), the server uses
`ONPREM_FOUNDRY_CACHE_DIR`. Both are `D:\FoundryCache` here.

## Risks

- **Daemon not installed / not running.** Chat degrades to an error; the rest of the server boots.
  Startup logs say which backend is active.
- **Tool-call envelope differences.** Forced `tool_choice` is verified working against the daemon,
  but the planner is strict about its JSON; the `first_json_object` tolerance added for the stray
  closing brace stays relevant.
- **Model cache split.** The daemon reads `~/.foundry/config.json` (`cache_directory`), the server
  reads `ONPREM_FOUNDRY_CACHE_DIR`. Both must point at the same directory or models appear absent
  on one side. Currently both are `D:\FoundryCache`.
- **Streaming cancellation.** Dropping the HTTP response must not leave the daemon generating; the
  client disconnect path in `agents/routes.rs` needs a check.
