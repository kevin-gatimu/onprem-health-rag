
# Rebuild `onprem-rag-app` UI — mobile-first, ported from the Electron reference

## Context

`onprem-rag-app/` is currently a 4-tab scaffold (~2.7k lines) with inline styles: a `useState<Tab>`
switch in [App.tsx](onprem-rag-app/src/App.tsx), a React context for session, and one hand-rolled
log store. It works, but it is not the product.

A **complete, working implementation of the same product already exists** at
`On-premise-Rag-system-for-Health-Records/` — an Electron app with 14 routes and ~14.3k lines of
renderer code. The screenshots in `Appimages/` are from that app. The goal is to bring that UI across.

Three things make this a rebuild rather than a copy:

1. **Different backend boundary.** The reference keeps its backend *in-process* (Electron main,
   TypeScript, SQLite + DocumentDB + Foundry). The target splits it: a **Rust Rocket server** over
   HTTP, reached through the **Tauri bridge** which holds the JWT. Every `window.api.*` IPC call
   becomes a bridge command — and the Rust server is missing a lot of what the UI needs.
2. **Mobile-first, desktop + Android.** The reference is desktop-only — 8 media queries in the whole
   renderer, smallest breakpoint 1024px. Every screen needs a real small-screen composition, and
   `src-tauri` needs Android targets initialised.
3. **Different stack.** Tailwind v4 instead of SCSS modules; TanStack Query for server state; real
   Zustand (with middleware) instead of the reference's hand-rolled vanilla store.

Outcome: one React codebase shipping as a Windows desktop app and an Android app, matching
`Appimages/`, backed by the Rust server.

---

## Decisions

| Area               | Decision                                                                                                                                    |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| UI state           | **Zustand** — session, UI/nav, ingestion wizard, streaming buffers                                                                   |
| Server state       | **TanStack Query** — all reads, polling, pagination                                                                                  |
| Styling            | **Tailwind v4** (`@tailwindcss/vite`, CSS-first `@theme`)                                                                         |
| Navigation         | **Nav stack in the `ui` store**, no router dep (see below)                                                                          |
| Icons              | **lucide-react**                                                                                                                      |
| Mobile             | **Android** via `tauri android init`. iOS deferred (needs macOS)                                                                    |
| Connectivity       | Pre-login**"Connect to server"** screen; persist server URL **and** JWT                                                         |
| Identity           | Add**`email` + `name`** to the Rust `User` — confirmed by the Admin screen                                                     |
| Roles              | **Extend Rust to 4 roles** (admin/doctor/nurse/analyst) — no `readonly`. Port `ROUTE_PERMISSIONS` with the readonly tier dropped |
| Analytics / Alerts | Ship as**`StubPage`** (they are stubs in the reference too — no UI, no screenshots)                                                |
| Audit Log          | **Build for real** — Rust `write_audit` + `audit_log` collection + `GET /audit`                                                |
| Service controls   | Foundry read-only (in-process, no start/stop/restart); Re-register EPs action; DocumentDB read-only health + CLI guidance              |

---

## Design tokens

Dark-only, ported from the reference `_tokens.scss` into Tailwind v4 `@theme` in `src/styles/theme.css`.

```
Surfaces   bg-primary #0f1117 · bg-secondary #161b24 · bg-elevated #1e2533 · overlay rgba(12,15,20,.8)
Borders    border rgba(61,79,107,.3) · border-strong rgba(90,111,143,.5)
Text       primary #f0f4f8 · secondary #8297b5 · muted #5a6f8f · disabled #2a3347
Accent     accent #3b82f6 · accent-hover #60a5fa · accent-subtle rgba(59,130,246,.12)
Status     success #4ade80 · warning #fbbf24 · danger #f87171  (+ 10%-alpha bg variants)
Radii      sm 4 · md 8 · lg 12 · xl 16 · full 9999
Shadows    sm 0 1px 3px rgba(0,0,0,.3) · md 0 4px 12px rgba(0,0,0,.4) · lg 0 8px 32px rgba(0,0,0,.5)
Charts     --chart-series-1..8 (#60a5fa #2dd4bf #fbbf24 #a78bfa #4ade80 #f472b6 #38bdf8 #fb923c)
```

Two deliberate changes from the reference:

- **Type scale.** The reference uses `clamp(px, vw, px)` fluid sizes tuned for ≥1024px; at 360px they
  clamp to their floor and read too small. Use a fixed mobile-first scale (13/14/16/18/22) stepping
  up at `md:`.
- **Spacing.** Keep the 4px scale; page padding becomes `p-4 md:p-6 lg:p-8 xl:p-12`.

Keep `--chart-*` as `:root` custom properties — `AgentChart` reads them via `getComputedStyle`, so
they are a runtime dependency, not decoration.

---

## Responsive strategy

The reference shell is a 240px sidebar collapsing to a 60px rail at 1280px via a JS resize listener.
Mobile-first replaces it with three compositions:

| Breakpoint          | Navigation                                                                                                                          | Content                                 |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------- |
| `< 768px`         | Top app bar +**bottom tab bar** (Dashboard · Data · Chat · Agents · More); "More" opens a sheet with the full 14-item nav | Single column,`p-4`, safe-area insets |
| `md: 768–1279px` | 60px icon rail with tooltips                                                                                                        | Two-pane where it helps                 |
| `xl: ≥1280px`    | Full 240px sidebar, three labelled sections                                                                                         | Desktop layouts from`Appimages/`      |

Per-screen mobile compositions — the net-new design work:

- **Tables → cards.** Below `md`, Admin users / Audit log / ingestion history render as stacked
  label-value cards, not `<table>`.
- **Data Explorer.** Desktop is stat strip + connection tree + wide grid. Mobile: stat strip (2×2) →
  table picker in a drawer → **row cards** showing the first 3 columns, tap to open the *existing*
  Row Detail drawer. No horizontal scrolling of a 12-column grid on a phone.
- **Chat / Agents.** Conversation sidebar and the Agents tools panel become drawers. Composer sticky
  above `env(safe-area-inset-bottom)`. Agent tab strip scrolls horizontally.
- **Ingest wizard.** Already step-based; the `ingesting` split view stacks vertically.
- **Login.** Both variants already specified — the ECG brand panel is `hidden lg:block`.

---

## Client architecture

### Navigation — a stack in the `ui` store

14 routes with role guards, but no URL bar (the reference itself uses `createMemoryHistory`). A
router buys little and costs ~15 kB.

```ts
// stores/ui.ts
navStack: Route[]     // ['dashboard'] → ['dashboard','data']
navigate(route)       // push (replace if same)
back()                // pop; no-op at depth 1
```

Android's hardware back button wires to `back()` — this is why nav is a stack, not a flat enum.

Port `canAccess(path, role)` from the reference's `src/shared/permissions.ts`; one matrix drives both
the route guard and sidebar filtering. With `readonly` dropped, the tiers collapse to three:

| Access                                 | Routes                                                                          |
| -------------------------------------- | ------------------------------------------------------------------------------- |
| `admin` only                         | `/connections` · `/ingest` · `/audit` · `/admin`                     |
| `admin` · `doctor` · `analyst` | `/agents` · `/analytics` · `/alerts`                                    |
| all roles                              | `/` · `/data` · `/chat` · `/models` · `/settings` · `/profile` |

The reference's "everyone except readonly" tier (`/chat`, `/models`, `/settings`) becomes simply "all
roles". Pages stay `React.lazy` + `<Suspense>`.

### State split

**Zustand** (`src/stores/`) — client state only:

| Store         | Holds                                                                                                            |
| ------------- | ---------------------------------------------------------------------------------------------------------------- |
| `session`   | user, auth status, server URL, connection state. Replaces[useSession.tsx](onprem-rag-app/src/auth/useSession.tsx) |
| `ui`        | navStack, sidebar collapsed, drawers/sheets, toasts,`previewRole`                                              |
| `ingestion` | the wizard's workflow state — must survive navigation. Port the reference's shape                               |
| `stream`    | chat/agent token buffers, ingest log ring (cap 500), service console ring (cap 300)                              |

Middleware the reference lacks and we want: `persist` (sidebar collapsed, last connection id —
replaces its `electron-store` IPC round-trip) and `subscribeWithSelector`.

**TanStack Query** — everything read from the server:

- The ~8 pages doing `useEffect` + `useState` + a manual `let cancelled = false` → plain `useQuery`.
- The two hand-rolled SWR stores (`models.store.ts`, `data-explorer.store.ts` — literally
  `{data, loaded, refreshing, lastLoaded}` + `ensureXLoaded(maxAgeMs = 30_000)`) → `staleTime: 30_000`.
- All four independent polls (services 10 s, Settings 8 s, Models 12 s, Foundry/Docker 15 s) →
  `refetchInterval`, deduped by query key instead of four `setInterval`s.
- Data Explorer pagination → `placeholderData: keepPreviousData`.

### Streaming — the one real perf fix

The reference appends **one React `setState` per token**, cloning the whole message array each time;
its Ingest page needs 15 narrow selectors just to survive un-batched progress events (its own
comments say so). Do not carry this over.

Single `src/lib/bridgeEvents.ts` registers **all** Tauri listeners once at boot and fans them into
stores — no per-component `listen`/`unlisten` (the leak-prone pattern in
[bridge.ts](onprem-rag-app/src/lib/bridge.ts)). Tokens accumulate in a mutable buffer written from the
listener and flush to the store on `requestAnimationFrame`: a 60-token burst becomes one paint, not
60 renders. Same ring-buffer treatment for ingest logs and the service console. Correlate by
client-generated `runId` so stale runs are dropped.

### Bridge and connectivity

`src-tauri` keeps base URL + JWT in an in-memory `Mutex` ([state.rs](onprem-rag-app/src-tauri/src/state.rs))
— both lost on restart, fatal on Android where the OS kills backgrounded apps.

- Persist server URL + JWT via `tauri-plugin-store`.
- New pre-login **"Connect to server"** screen: URL entry → `GET /health` probe → Login.
- `tauri android init`; verify the `reqwest` + `eventsource-stream` SSE relay works on Android
  (cleartext HTTP to a LAN IP needs an Android network-security config).

> ⚠️ Every new server response field must be mirrored in **both**
> [commands.rs](onprem-rag-app/src-tauri/src/commands.rs) **and** [bridge.ts](onprem-rag-app/src/lib/bridge.ts),
> or it is silently dropped.

---

## Component inventory

`src/components/ui/` — rebuilt in Tailwind, same APIs (they're good): `Button` (variant × size,
`loading`, `full`, left/right icon) · `Input` (label/hint/error/icon/`showToggle`) · `Select` ·
`Badge` · `Modal` (**add a focus trap and a mobile bottom-sheet variant**) · `Toast` · `StubPage` ·
plus `Card` / `StatCard` / `EmptyState`, currently duplicated inline across reference pages — extract.

Ported near-verbatim (they carry real logic):

- **`EcgLogo`** — two-cycle SVG path scrolled by CSS `translateX(-28px)` at 1.4 s, gradient-masked.
  Use `useId()` for the SVG ids instead of the hardcoded strings.
- **`LoginBackground`** — 26-cycle ECG polyline via `d3-shape`, three traces at different speeds,
  ECG-paper `<pattern>` grid, phosphor glow filter. Pure CSS animation.
- **`AgentChart`** (458 lines, d3 submodule imports) + `chart-schema.ts` (Zod) +
  `markdown-components.tsx` — 8 chart types, `ResizeObserver`, reads `--chart-*`.
- **`AgentActivity`** — collapsible live tool-call feed.

Reference bugs to fix while porting: `ToastContainer` mounted twice (AppShell + main);
`app-loader-spinner` class referenced but never defined; `Login.module.scss` broken indentation.

New deps: `tailwindcss` + `@tailwindcss/vite`, `zustand`, `@tanstack/react-query`, `lucide-react`,
`react-markdown` + `remark-gfm`, `d3-{selection,array,scale,shape,format}`, `zod`,
`@tauri-apps/plugin-store`.

---

## Server-side work (`onprem-rag-server`)

The Rust server has 21 routes; the UI needs substantially more. Grouped by the screen that forces it.

**Identity & auth** — `src/auth/`

- `User`: add `email` (unique index), `name`, `updated_at`. `#[serde(default)]` so existing docs
  still deserialize; backfill `email` from `username` on boot in [seed.rs](onprem-rag-server/src/auth/seed.rs).
- `Role`: extend to `admin|doctor|nurse|analyst`; include in JWT claims ([jwt.rs](onprem-rag-server/src/auth/jwt.rs)).
  Existing `user`-role docs migrate to `doctor` (the least-restricted non-admin tier).
- `POST /auth/login`: accept an `identifier` matching email **or** username (`#[serde(alias = "username")]` keeps the bridge working).
- New: `GET /auth/users`, `PATCH /auth/users/<id>`, `DELETE /auth/users/<id>` (**last-admin guard**),
  `POST /auth/users/<id>/password`, `PATCH /auth/me`, `POST /auth/me/password` (verifies current).

**Dashboard** — new `GET /stats` → `{total_records, total_tables, last_ingest_at, active_connections, pending_alerts, llm_status}`. All inputs exist server-side.

**Connections** — `PATCH /sources/<id>`, `DELETE /sources/<id>`; add `status` / `last_connected` /
`error` to `SourceInfo`. Port the reference's `toDbSchemaErrorMessage` mapping (ECONNREFUSED / 28P01 /
ENOTFOUND / ETIMEDOUT → human text) or the Connections page loses its error UX.

**Ingest** — the biggest lift. `SourceConnector` gains `get_schema()` per driver (Postgres/MySQL/MSSQL)
→ `GET /sources/<id>/schema` returning `TableSchema[]` with per-column type/nullable/PK/FK/`likely_pii`.
`POST /ingest` widens from `{source_id, limit}` to `{source_id, tables[], excluded_columns}`.
The SSE progress event grows from 4 fields to the reference's 16 (`current_table`, `table_index`,
`success_tables`, `db_size_bytes`, `log_entry`, …). New `indexed_tables` collection to back ingestion
history + drift (`GET /ingest/history`, `GET /ingest/drift/<source_id>`). New `POST /schema/analyze`
(deterministic PII keyword pass + LLM JSON pass — Rust already has `plan_aggregation` as a model).

**Data Explorer** — the largest gap. Rust `records` docs are **chunk-grained**; the grid is
**row-grained**. Add a `raw_data`-equivalent (or a distinct-on-`row_pk` projection) plus:
`GET /records?source_id&page&page_size&q`, `GET /records/<id>`, `GET /sources/<id>/overview`,
`GET /tables/<id>/info`, `DELETE` variants for table / connection / all, `POST /cache/clear`,
`POST /reindex`. Also `table_profiles` and `ingestion_runs` collections.

**Chat & Agents** — `chat_conversations` + `chat_messages` collections with
`GET/POST/DELETE /conversations`, `GET|POST /conversations/<id>/messages`, auto-titling from the first
user message, and an agent-partitioned variant. Scope by **JWT user id** — the reference hardcodes
`userId: 'default'` and is effectively single-tenant; don't copy that. Add `run_id` correlation and
pipeline-step activity events to the `/chat` SSE so the live status strip has a source. Add `'auto'`
agent routing.

**Models & Settings** — in-process Foundry native singleton (no start/stop/restart);
`POST /models/unload` (per-variant memory free); composite `GET /setup-status` (hardware + gpu + active
model + foundry_endpoint="in-process (native SDK)" + service statuses + loaded/cached models) + `POST
/hardware/register-eps` (Re-register execution providers admin action); Settings one call, not a fan-out.
Make `POST /models/pull` admin-guarded for consistency with `/models/select`.

**Audit Log** — `write_audit()` helper + `audit_log` collection (reuse the shape the reference
already declared but never wrote: `{user_id, action, resource, details, timestamp}`), wired into
auth login/logout/failure, user CRUD, source CRUD, ingest. Read side `GET /audit?user&action&from&to&page`.

**Not doing:** Docker lifecycle routes (the server depends on the container it would start —
read-only health + `docker compose up -d documentdb` guidance instead); MongoDB as a *source* kind;
the eval harness; `agent_memory` / `semantic_cache` / `knowledge_entities` collections (reference RAG
features with no Rust pipeline stage to consume them).

**Migration hazard:** reference password hashes are scrypt `salt:hex` (Better Auth); Rust uses argon2.
Not portable — any user migration needs a forced reset.

---

## Build order

Each stage lands one screen end-to-end (server route → bridge → store/query → responsive UI), verified
on desktop and at 360px before moving on.

| #  | Stage                                                                                                                                                                                                                             | Server work                                                                                                                         |
| -- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| 0  | **Foundation** — Tailwind v4 + tokens, Zustand stores, Query provider, `bridgeEvents.ts`, UI primitives, responsive AppShell (sidebar / rail / bottom-bar + sheet), `tauri android init`, `plugin-store` persistence | 5-role enum,`email`/`name` on `User`, `write_audit` helper                                                                  |
| 1  | **Connect to server + Login**                                                                                                                                                                                               | login by email-or-username, seed + unique index                                                                                     |
| 2  | **Dashboard** (+ Preview-as)                                                                                                                                                                                                | `GET /stats`                                                                                                                      |
| 3  | **Connections**                                                                                                                                                                                                             | `PATCH`/`DELETE /sources`, status fields, error mapping                                                                         |
| 4  | ✅ **Ingest wizard** (2026-08-25)                                                                                                                                                                                              | `GET /sources/<id>/schema`, widened `POST /ingest`, 14-field SSE, `indexed_tables`, history + drift, `POST /schema/analyze` |
| 5  | ✅ **Data Explorer** (2026-08-25)                                                                                                                                                                                                            | row-grained store + browse/search/paginate/inspect/clear-all routes (reindex and drift dropped; overview totals client-side) |
| 6  | **AI Chat**                                                                                                                                                                                                                 | conversations + messages collections and routes,`run_id`, activity events                                                         |
| 7  | **AI Agents**                                                                                                                                                                                                               | `'auto'` routing, agent-partitioned conversations, agent context                                                                  |
| 8  | ✅ **Models + Settings** (2026-08-25)                                                                                                                                                                                                       | `GET /setup-status` (degraded-safe), `POST /models/unload`, `POST /hardware/register-eps`; no Foundry lifecycle                                                                              |
| 9  | **Profile + Admin**                                                                                                                                                                                                         | user CRUD, password routes,`PATCH /auth/me`                                                                                       |
| 10 | **Audit Log**                                                                                                                                                                                                               | `GET /audit` + write-path wiring                                                                                                  |
| 11 | **Analytics + Alerts stubs, polish, Android build**                                                                                                                                                                         | —                                                                                                                                  |

Stages 4 and 5 are the heavy ones; expect each to be roughly the size of stages 1–3 combined.

Stage 2 note: the reference Dashboard has a readonly-specific path (single-column layout, Foundry
banner hidden, reduced stat set). Drop it — with `readonly` gone, every role gets the full two-column
Dashboard, filtered only by the stat/quick-action role rules.

---

## Verification

**Per stage**

- `cargo build` in `onprem-rag-server`; exercise new routes from [server.http](server.http) with a real JWT.
- `npx tsc --noEmit` and `npm run tauri dev` in `onprem-rag-app`.
- Resize the dev window to **360 × 640** and confirm the screen is usable — no horizontal body
  scroll, tap targets ≥44px, bottom bar clear of the composer.
- New response fields: grep both `src-tauri/src/commands.rs` and `src/lib/bridge.ts` to confirm the
  field is mirrored in each.

**End-to-end, once stages 0–5 land**

```bash
docker compose up -d documentdb                 # wait for "bound to port 10260"
docker compose --profile dev-sources up -d      # seeded Postgres source
cd onprem-rag-server && cargo run               # http://0.0.0.0:8000
cd onprem-rag-app && npm run tauri dev
```

Walk: connect → log in as `admin` → add the seeded Postgres connection → load schema → select tables,
exclude a PII column → ingest and watch live progress → browse the rows in Data Explorer → ask a
grounded question in AI Chat and confirm citations.

**Streaming perf** — React DevTools Profiler during a chat response: token bursts should show
~1 commit per frame, not one per token. Check the ingest log ring stays capped at 500.

**Android** — `npm run tauri android dev` against a physical device on the same LAN; point the
Connect screen at `http://<host-lan-ip>:8000`. Verify: SSE streams arrive, hardware back pops the nav
stack, JWT survives backgrounding the app, and cleartext HTTP is permitted by the network-security config.
