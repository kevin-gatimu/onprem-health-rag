# Stage 8 — Models + Settings (implementation contract)

Status: **IMPLEMENTED** (2026-08-25). Opus-authored spec; Sonnet delivered layer-by-layer; Opus verified all diffs.

Screen(s): **System Setup** (`/settings`) and **Models** (`/models`). Both are `ALL`-role routes
(already in the Stage-0 permission matrix + nav registry — no nav/permission edits needed).

## Governing reshapes (the reference does NOT map cleanly — implement the correct way)

Our Foundry is an **in-process native `&'static FoundryLocalManager`** initialized once at boot
(see `foundry/mod.rs`; "ChatClient calls the core directly — no HTTP endpoint, no dynamic port").
The Electron reference instead shells out to an external `foundry service` CLI. Consequences:

1. **No Start / Stop / Restart service lifecycle.** There is no external process to start or kill.
   `state.foundry()` is either `Ok` (native core initialized) or `Err` (native libs failed at boot).
   - **Honest analog of "restart":** re-run EP discovery/registration via the EXISTING
     `POST /hardware/register-eps` (admin). Surface Foundry as a read-only status + a single
     **"Re-register execution providers"** admin action. **No start/stop/restart buttons.**
2. **No `foundryEndpoint` URL.** The core has no port. Report the literal string
   `"in-process (native SDK)"` when ready, `""` when down.
3. **No Docker lifecycle.** DocumentDB is read-only health + CLI guidance text
   (`docker compose up -d documentdb`). No start/stop.
4. **No startup "download preferred model" broadcast modal.** That was an Electron main-process
   broadcast. Drop it. Keep a static "no chat model downloaded yet" hint + a Download CTA.
5. **Consolidated model actions** (reference had separate download/load/unload IPCs):
   - **Download** = existing `POST /models/pull` (SSE progress, `load:false`).
   - **Load + select** = existing `POST /models/select` (download-if-needed → load → set current).
   - **Unload** = NEW `POST /models/unload` (this stage).
   - **Delete weights** = existing `POST /models/delete`.
   - **Set role default** = existing `PUT /settings/router`.

## What already exists (reuse — do NOT rebuild)

Server routes: `GET /hardware`, `POST /hardware/register-eps`, `GET /models`, `GET /models/roles`,
`POST /models/select`, `POST /models/pull` (SSE), `POST /models/delete`, `PUT /settings/router`,
`POST /generate` (SSE test).
Bridge (commands.rs + bridge.ts): `get_hardware`/`getHardware`, `list_models`/`listModels`,
`select_model`/`selectModel`, `register_eps`/`registerEps`, `model_roles`/`getModelRoles`,
`pull_model`/`pullModel` (SSE callbacks: onProgress/onStatus/onError/onDone via `model://*`),
`delete_model`/`deleteModel`, `set_router` command. **Verify a `setRouter` wrapper exists in
`bridge.ts`; add it if missing** (the Rust command exists at commands.rs:~323).
App: `stores/stream.ts` holds `serverLog: LogLine[]` (+ `appendServerLog`/`clearServerLog`, cap 300);
`bridgeEvents.ts` already relays `logs://line` into it at boot. No ServiceConsole UI exists yet.

---

## Layer 1 — Server (`onprem-rag-server`)

### 1a. `POST /models/unload` (admin) — new route in `foundry/routes.rs`
- Body: `UnloadModelReq { variant_id: String }`. Response: `Json(json!({ "ok": true }))`.
- New manager method `FoundryManager::unload_model(&self, variant_id: &str) -> AppResult<()>`:
  mirror the FRONT half of `delete_model` (resolve variant → `model.unload().await`) but **do NOT
  delete weights**; prune the id from `lru_residents`. A failed unload logs a warning and still
  returns Ok (idempotent — unloading a not-resident model is a no-op success), matching how
  `delete_model` treats unload failures as non-fatal.
- `user.require_admin()?`.

### 1b. `GET /setup-status` (any authed user) — new route in `foundry/routes.rs`
One call powering the whole Settings page. **Must return 200 even when Foundry is down** (degraded
state), so read GPU from the OS-level `crate::foundry::hardware::detect()` (independent of the core),
and only touch `state.foundry()` behind `if let Ok(f)`.

```rust
#[derive(Serialize)] pub struct GpuInfo { pub has_gpu: bool, pub gpu_name: Option<String> }
#[derive(Serialize)] pub struct ServiceStatus { pub name: String, pub status: String, pub detail: Option<String> } // status: "ok"|"error"|"unknown"
#[derive(Serialize)] pub struct SetupStatus {
    pub gpu: GpuInfo,
    pub active_chat_model: String,          // foundry.current_model() if up, else ""
    pub foundry_endpoint: String,           // "in-process (native SDK)" if up, else ""
    pub foundry_ready: bool,                // state.foundry().is_ok()
    pub services: Vec<ServiceStatus>,
    pub loaded_models: Vec<String>,         // list_models() filtered loaded==true (empty if foundry down)
    pub cached_models: Vec<String>,         // list_models() filtered cached==true (empty if foundry down)
    pub execution_providers: Vec<ExecutionProvider>, // foundry.hardware().execution_providers (empty if down)
}
```
- `gpu`: from `hardware::detect()` — `has_gpu` = any device `kind=="GPU"`; `gpu_name` = that device's `name`.
- `services` (build in this order):
  - `"Foundry Local"` → ok if `state.foundry().is_ok()` else error; detail = active model, or
    `"native core unavailable"` when down.
  - `"DocumentDB"` → ok if `state.db.ping().await.is_ok()` else error; detail `"MongoDB-wire / pgvector"`.
  - `"Embeddings (fastembed)"` → ok; detail = `state.config.embedding_model`.
  - `"Reranker (fastembed)"` → ok; detail = `state.config.rerank_model`.
- `loaded_models`/`cached_models`/`execution_providers`: only populated when `state.foundry()` is Ok
  (call `list_models().await` once, partition; call `hardware()` for EPs). When down, all empty.

### 1c. Admin-guard `POST /models/pull`
Change `_user: AuthUser` → `user: AuthUser` + `user.require_admin()?` (consistent with select/delete).

### 1d. Mount
Add `foundry::routes::unload_model` and `foundry::routes::setup_status` to the existing foundry
`.mount("/", routes![...])` block in `main.rs`.

**Verify:** `cargo check` (host cannot `cargo run`/`build` — Smart App Control, OS error 4551).

---

## Layer 2 — Bridge (`onprem-rag-app/src-tauri` + `src/lib/bridge.ts`)

Mirror-hazard: every field in BOTH `commands.rs` and `bridge.ts` or it is silently dropped.

- `commands.rs`: add `GpuInfo`, `ServiceStatus`, `SetupStatus` mirror structs (snake_case, serde);
  `#[tauri::command] get_setup_status` (GET `/setup-status`) and `unload_model(variant_id: String)`
  (POST `/models/unload`, body `{variant_id}`). Register both in `lib.rs`.
- `bridge.ts`: add `GpuInfo`/`ServiceStatus`/`SetupStatus` interfaces; `getSetupStatus(): Promise<SetupStatus>`
  and `unloadModel(variantId: string): Promise<void>` (invoke arg camelCase `variantId`).
- If `bridge.ts` lacks a `setRouter(role, variantId|null)` wrapper for the existing `set_router`
  command, add it (`invoke("set_router", { role, variantId })`).

**Verify:** `cargo check` + `npx tsc --noEmit`.

---

## Layer 3 — App (two features; ServiceConsole shared, built in 3a, reused in 3b)

Registry: **Opus adds** the two lazy lines to `app/routes.tsx` (`'/settings'`, `'/models'`) after the
bridge lands — agents do NOT edit routes.tsx (avoids a shared-file conflict). Mobile-first: design at
360px, layer `md:`/`xl:` up; tap targets ≥44px; no horizontal body scroll.

### 3a. `features/settings/` + shared `components/ServiceConsole.tsx`
- **`components/ServiceConsole.tsx`** (shared): compact activity panel reading
  `useStream(s => s.serverLog)`; monospace, newest-at-bottom auto-scroll, level-colored, capped list;
  "Clear" button → `clearServerLog()`. Small, self-contained.
- **`features/settings/index.tsx`** (`Settings` default export): TanStack Query `['setup-status']`
  with `refetchInterval: 8000`, `staleTime: 30_000`. Sections (stack on mobile, `md:` 2-col where noted):
  1. **Header** + Refresh (invalidate `['setup-status']`).
  2. **Hardware** card — GPU name (or "CPU-only mode") + `active_chat_model`.
  3. **Service Health** grid — one card per `services[]` (status dot ok/error/unknown + name + detail).
     Show `foundry_endpoint` as a code hint when non-empty.
  4. **Foundry** card — ready/down; when admin, a **"Re-register execution providers"** button →
     `registerEps()` (toast result, invalidate). DocumentDB guidance line: if the DocumentDB service
     is error, show `docker compose up -d documentdb`.
  5. **Active Chat Model** — current + a switch `<Select>` over `[...cached, ...loaded]` (deduped,
     loaded flagged) → `selectModel(id)` (admin only; non-admins see read-only current). "Manage
     Models" button → `navigate('/models')` (ui store).
  6. **Loaded Models** — chips from `loaded_models`; admin **"Unload all"** → iterate `unloadModel`
     over loaded ids (reference pattern), then invalidate.
  7. **Activity** — `<ServiceConsole />`.
- Non-admins: hide mutating controls (switch/unload/re-register) — read-only view. Gate on
  `useSession` real role (not previewRole).

### 3b. `features/models/` (reuses `components/ServiceConsole`)
- **Catalog source = `getModelRoles()`** (our real model config), NOT a reinvented family catalog.
  Query `['model-roles']` (`staleTime 30_000`) + `['setup-status']` (`refetchInterval 12_000`) for
  live loaded/ready state. Also `getModelRoles` variants already carry `cached`/`loaded`/`current`.
- **Foundry status bar**: ready/down (from setup-status `foundry_ready`) + **Re-register EPs** (admin)
  + Refresh. **No start/stop/restart** (reshape #1).
- **Role sections**: iterate roles. Managed roles (`managed:true`) render a variant grid; non-managed
  roles (embeddings/reranker) render a single read-only status row (engine/device/model). Show each
  role's `override_variant` as the current default; a **"Set as default"** action per variant →
  `setRouter(role, variantId)` (admin), and clear via `setRouter(role, null)`.
- **VariantCard** (per `VariantInfo`): device badge (derive from id tokens: `gpu`/`npu`/`cpu`/`openvino`
  → Zap/Microchip/Cpu), pills for `current`/`loaded`/`cached`, model id + copy, context length.
  Action by state (admin-gated): not cached → **Download** (`pullModel(id,false,cb)` with per-variant
  progress bar from `onProgress`/`onStatus`; on done invalidate `['model-roles']`+`['setup-status']`);
  cached & not loaded → **Load** (`selectModel(id)`); loaded → **Unload** (`unloadModel(id)`) +
  **Delete** (`deleteModel(id)`, confirm; surfaces `cleared_roles`). Non-admins: copy only.
- **Activity**: `<ServiceConsole />`.
- Empty/degraded: if `foundry_ready` false, show a clear "Foundry Local core unavailable" banner and
  keep the role manifest visible (roles come from config even when the core is down — but variants
  needing `state.foundry()` may be empty; handle gracefully).

**Verify (each app agent):** `npx tsc --noEmit` exit 0.

---

## Deliberate deviations (record in progress memory on completion)
1. No Foundry start/stop/restart — in-process core; "Re-register EPs" is the honest re-init action.
2. `foundry_endpoint` = "in-process (native SDK)" string, not a URL/port.
3. No Docker lifecycle — read-only DocumentDB health + `docker compose up -d documentdb` guidance.
4. No startup "preferred model missing" broadcast/modal — static hint + Download CTA.
5. Models catalog is driven by `GET /models/roles` (our real role→variant config), not a reinvented
   family catalog; embeddings/reranker shown read-only (fastembed, not Foundry-managed).
6. Unload is per-variant server-side; "Unload all" is a client loop (reference pattern).
7. Set-as-default uses the existing per-role `PUT /settings/router` override.

## Verification gate (Opus, per layer — read the actual diff, re-run checks myself)
- L1: `cargo check` clean; routes mounted; pull is admin-guarded; setup-status returns 200 when
  foundry is down (degraded path reads `hardware::detect()` not `foundry.hardware()`).
- L2: mirror structs field-identical in commands.rs + bridge.ts; `cargo check` + `tsc` clean.
- L3: `tsc` exit 0; no start/stop/restart buttons; admin-gating on all mutations; ServiceConsole
  reads the existing stream ring; 360px usable.
