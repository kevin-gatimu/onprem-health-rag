# 15 — Stage 11: Analytics/Alerts stubs, polish, docs, Android — implementation contract

**STATUS: IMPLEMENTED (2026-08-25)** — Analytics/Alerts honest stubs + global ErrorBoundary wired (app tsc exit 0); Android manifest cleartext forced on; server README API table (49 routes) + CORS/TLS note, app README Android section, and root README all landed and Opus-verified against source. Android build/run + hardware-back deferred to on-device work (host build blocked by Smart App Control / OS error 4551).

The final stage. Not a new screen — a close-out sweep: ship the two deliberate stub screens honestly,
harden the app against render crashes, guarantee Android can reach the on-prem server over LAN HTTP, and
bring the documentation current. The heavy construction is done and `src-tauri/gen/android/` already exists
(`tauri android init` was run in a prior session); this stage does **not** re-run init.

Ground truth verified before authoring (cite, do not re-derive):

- `/analytics` + `/alerts` already exist in `app/navigation.ts` (§Analytics), `lib/permissions.ts`
  (`["admin","doctor","analyst"]`), and the `Route` union (`lib/types.ts`). They are **absent from the
  `routes.tsx` registry**, so they render the generic fallback `StubPage` ("This screen is being rebuilt —
  coming in the next phase"). That copy is wrong for permanent planned features.
- `StubPage` props: `{ title: string; description?: string; icon?: ReactNode }`; it already renders a
  "Coming in next phase" pill. Reuse it — no new primitive.
- `main.tsx` mounts `QueryClientProvider > BridgeEvents + App + Toasts`. **No React ErrorBoundary anywhere** —
  a render-time throw white-screens the whole app.
- `queryClient.ts`: `staleTime 30_000`, `refetchOnWindowFocus:false`, `retry:1`. No global error handler.
  Per-screen `isError` handling + per-mutation `onError→toast` already exist widely, so a **global**
  MutationCache/QueryCache onError would double-toast — do **not** add one. The gap is the ErrorBoundary only.
- Android manifest (`gen/android/app/src/main/AndroidManifest.xml`) has `INTERNET` permission and
  `android:usesCleartextTraffic="${usesCleartextTraffic}"` (a gradle placeholder, false in release by default).
  On-prem reaches the server at `http://<lan-ip>:8000` (plain HTTP), so cleartext must be **forced on**.
  There is **no** `network_security_config.xml`.
- Hardware back: only *comments* in `stores/ui.ts` + `src/README.md` mention it. **Nothing wires the Android
  back button to `back()`.** Tauri v2 has no stable JS API for the Android hardware back button → document as
  an on-device follow-up; do NOT ship an untested handler.
- Server has **no CORS fairing** (grep clean). Correct: the Tauri bridge calls the server with native
  `reqwest`, not a browser `fetch`, so CORS never applies. This is a documentation note, not a code gap.
- Server `main.rs` tracing is already complete (EnvFilter + fmt layer + `logstream::BroadcastLayer`). No
  tracing code change needed — the "tracing" polish item is a README mention.
- Server README's API table is stale (pre-WS5): it lists ~7 route groups but the server now mounts far more
  (audit, stats, conversations, agents, schema, setup-status, PATCH/DELETE sources, unload, etc. — see
  `main.rs` mount blocks). App README has no Android section. There is no repo-root README (only `CLAUDE.md`).

---

## Governing decisions / deviations

1. **Analytics + Alerts stay stubs** (as in the reference — no backend, no screenshots). Ship dedicated
   `features/analytics` + `features/alerts` modules that render `StubPage` with *honest, specific* copy
   (what the feature will do), not the generic "being rebuilt" fallback.
2. **Error surface = a single global ErrorBoundary.** No global query/mutation error handler (would
   double-toast the existing per-call handlers). The boundary catches render throws and offers a reload.
3. **Android cleartext is forced on in the manifest** (`usesCleartextTraffic="true"`), not gated to debug —
   on-prem is LAN HTTP in production too. Minimal, well-understood one-line change; the unused gradle
   placeholder is harmless.
4. **Hardware back is documented, not coded** — no stable Tauri v2 API; shipping an untested listener would
   be dishonest given this host cannot build/run Android (Smart App Control, OS error 4551).
5. **No CORS code, no TLS code** — both are deployment notes in the server README (why no CORS; how to
   terminate TLS via a reverse proxy for production; DocumentDB's self-signed TLS is already handled).
6. **CLAUDE.md is left as-is** — it is current project guidance; the master plan tracks stage status.

---

## Layer A — App code (`onprem-rag-app/src`) — **Opus implements directly** (small, and touches shared files)

### A1. Stub screens
- `features/analytics/index.tsx` — default export, zero-prop, renders
  `<StubPage title="Analytics" icon={<BarChart3 size={32}/>} description="Cohort trends, ingestion
  throughput, and retrieval-quality dashboards will live here. The data pipeline that feeds them
  (structured aggregation over ingested records) is already in place via the AI Agents screen." />`.
- `features/alerts/index.tsx` — default export, zero-prop,
  `<StubPage title="Outbreak Alerts" icon={<Bell size={32}/>} description="Rule- and anomaly-based alerts
  over incoming records (e.g. reportable-condition spikes) will surface here. No alert backend is wired
  yet — this is a planned capability." />`.
- Wire both into `app/routes.tsx` after `/audit`:
  `'/analytics': lazy(() => import('../features/analytics')) as ComponentType,` and
  `'/alerts': lazy(() => import('../features/alerts')) as ComponentType,`.

### A2. ErrorBoundary
- `components/ErrorBoundary.tsx` — a small class component (`{ hasError, error }` state,
  `getDerivedStateFromError`, `componentDidCatch` logging to console) rendering a centered dark-token
  fallback (icon, "Something went wrong", the error message in a muted `<pre>`, a **Reload** button →
  `window.location.reload()`). Themed with `text-fg`/`text-fg-muted`/`bg-base`/`Button`.
- Wrap `<App/>` in `main.tsx`: `<ErrorBoundary><App/></ErrorBoundary>` (inside `QueryClientProvider`, so
  the fallback can still use Toasts if needed; keep `BridgeEvents` + `Toasts` siblings as-is).

### A3. Verify (Layer A)
`cd onprem-rag-app && npx tsc --noEmit` → exit 0.

## Layer B — Android manifest — **Opus implements directly** (one line, untestable here)
- In `gen/android/app/src/main/AndroidManifest.xml`, change
  `android:usesCleartextTraffic="${usesCleartextTraffic}"` → `android:usesCleartextTraffic="true"` with a
  short comment explaining on-prem LAN HTTP. No build possible on this host — correctness is by inspection.

## Layer C — Documentation — **delegate to a Sonnet agent**, Opus verifies
The agent MUST read the `#[get()]/#[post()]/#[patch()]/#[delete()]` attributes in the server route files to
get exact paths/methods (do not guess). Handler→file map: `routes/{health,logs,stats,explorer,audit}.rs`,
`auth/routes.rs`, `foundry/routes.rs`, `connectors/routes.rs`, `ingest/routes.rs`, `rag/routes.rs`,
`routes/conversations.rs`, `agents/routes.rs`; the authoritative mounted set is the `.mount(...)` blocks in
`main.rs`.

- **`onprem-rag-server/README.md`** — replace the stale API table with a current one grouped by concern
  (Health/Stats, Auth + Users, Foundry/Models, Sources, Ingest, Explorer/Audit, Retrieval/Chat,
  Conversations, Agents), noting which stream SSE and which are admin-only. Add a **"Deployment: CORS & TLS"**
  section: (a) no CORS fairing is needed because the desktop/Android client reaches the server via the Tauri
  bridge's native `reqwest`, not a browser — there is no `Origin`/preflight; add one only if a real browser
  SPA is ever pointed at it; (b) the server speaks plain HTTP on `0.0.0.0:8000` — for anything beyond a
  trusted LAN, terminate TLS at a reverse proxy (nginx/Caddy) in front of it; DocumentDB's own connection is
  already TLS (self-signed, `tlsAllowInvalidCertificates=true`).
- **`onprem-rag-app/README.md`** — add an **"Android"** section: `gen/android` is already initialised;
  `npm run tauri android dev` / `npm run tauri android build` (needs Android SDK + **NDK** with
  `NDK_HOME`/`ANDROID_HOME` set); point the app at the server's **LAN IP** (`VITE_DEFAULT_SERVER_URL=
  http://<host-lan-ip>:8000`, or via the in-app Connect screen) since `localhost` on the device is the phone
  itself; note cleartext HTTP is enabled in the manifest for this reason; note the **hardware back button is
  a known on-device follow-up** (no stable Tauri v2 JS API yet — the nav stack's `back()` exists but is not
  yet bound to the OS back gesture).
- **new `README.md` at repo root** — concise orientation: what the system is (on-prem health-records RAG,
  PHI stays local), the two components (`onprem-rag-app` Tauri client, `onprem-rag-server` Rocket backend)
  and the traffic flow (React → bridge/JWT → server → DocumentDB/Foundry/sources), a 4-step quickstart
  (`docker compose up -d documentdb` → Foundry Local → server → app) that **links** the two sub-READMEs for
  detail, and a pointer to `CLAUDE.md` + `plans/`. Keep it short; do not duplicate the sub-READMEs.

### Verify (Layer C)
Opus re-reads all three files, spot-checks 5–6 route rows against the actual `#[...]` attributes in source,
and confirms no fabricated routes. Docs are prose — no build gate, accuracy gate instead.

---

## Definition of done
- `/analytics` + `/alerts` render honest dedicated stubs (not the "being rebuilt" fallback), wired in routes.tsx.
- A global ErrorBoundary wraps the app; a render throw shows a reload fallback, not a white screen.
- Android manifest forces `usesCleartextTraffic="true"`.
- Server README API table current + CORS/TLS deployment note; app README Android section; new root README.
- `npx tsc --noEmit` exit 0. Hardware back documented as an on-device follow-up (not shipped).
- Each layer verified by Opus (read the code/docs + re-run tsc — not agent self-reports).
- **Not done on this host (build constraint):** actual Android build/run and hardware-back wiring — deferred
  to on-device testing after reboot; called out in the app README.
