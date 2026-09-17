# 14 — Audit Log (Stage 10) — implementation contract

**STATUS: IMPLEMENTED (2026-08-25)** — all three layers verified by Opus (server `cargo check` + app `npx tsc --noEmit` both exit 0); `/audit` wired into routes.tsx.

Read side of the audit trail: an admin-only, filterable, paginated **Audit Log** screen backed by a new
`GET /audit` route, plus completion of the **write-path wiring** so every security-relevant mutation lands
an `audit_log` entry. Everything else (collection, `AuditEntry` shape, `write_audit` helper) already exists
from WS0/Stage 9 — this stage does **not** touch `auth/audit.rs`.

Ground truth verified before authoring (do not re-derive — cite this):

- `auth/audit.rs`: `AuditEntry { user_id, username, action, resource, details: Option<Value>, timestamp: DateTime<Utc> }`
  (`details` has `#[serde(skip_serializing_if = "Option::is_none")]`). Helper
  `write_audit(db, user_id, username, action, resource, details: Option<Value>)` is best-effort (logs a warning
  on failure, never propagates). Entries are inserted with **no `_id`** → Mongo assigns a default **ObjectId**
  (unlike the UUIDv7 string `_id` collections).
- `documentdb/mod.rs`: `pub const AUDIT_LOG = "audit_log"`; `db.audit_log()` → `Collection<Document>`.
- **Actions already written:** `login`, `login_failed`, `user_updated`, `user_deleted`, `password_set`,
  `profile_updated`, `password_changed`.
- **Not written yet (this stage adds them):** `user_created`, `source_created`, `source_updated`,
  `source_deleted`, `ingest_started`.
- **No server `logout` route exists** — logout is a client/bridge token-drop (stateless JWT). See deviation #1.
- Pagination template to mirror exactly: `routes/explorer.rs::list_records` (`$facet {rows,total}`, page
  defaults, page-size clamp, `page_count`, `Paged*` response struct, `#[get("…?<a>&<b>")]` + `AuthUser` guard).
- Bridge GET-with-query template: `commands.rs::list_records` (manual query string via `encode_path_segment`,
  `bearer_auth`, `check_auth`, `error_body`, `resp.json::<T>()`). Client template: `bridge.ts` `RecordsPage`
  + `listRecords`. UI template: `features/data-explorer/RowsGrid.tsx` (TanStack `useQuery` + `keepPreviousData`
  + `staleTime: 30_000`; `md:hidden` cards / `hidden md:block` table; Prev/Next pagination).
- `/audit` already has: nav item (`navigation.ts` §System, `ClipboardList` icon), permission `["admin"]`
  (`permissions.ts:16`), `Route` union member (`types.ts:33`). It renders `StubPage` today (absent from the
  `routes.tsx` registry). **Only** a real feature page + a `routes.tsx` entry are missing on the app side.

---

## Governing decisions / deviations from the reference

1. **No `logout` audit.** There is no server logout route (bridge drops the JWT; JWT is stateless). The
   reference logged logout from its in-process session store; we have none. Out of scope — do not add a route.
2. **`user` filter = case-insensitive substring on `username`** (what an admin actually types), implemented as
   `$regex`/`$options:"i"`, *not* an exact `user_id` match.
3. **Action filter options are hard-coded on the client** (the known-action list below). No `/audit/actions`
   distinct-values endpoint — it would be one more route for a list we already know.
4. **`_id` is returned as a string** via an aggregation `$toString` stage, so the bridge/client never parse an
   ObjectId. It is used only as a stable React key (the log is read-only; no per-row actions).
5. **Date filters accept `YYYY-MM-DD` *or* full RFC3339.** `from` → `$gte`; a date-only `to` is made
   end-of-day-inclusive (next-day `$lt`), a timestamped `to` → `$lte`. Bad date strings → `400`.
6. **Transient tests are not audited** (`source/test`, `sources/<id>/test`) — noisy, not security-relevant.

---

## Layer 1 — Server (`onprem-rag-server`)

### 1a. New read route `GET /audit` — new file `src/routes/audit.rs`

Mirror `routes/explorer.rs` structure and idiom (snake_case end-to-end, `AuthUser` guard, `$facet`).

**Route signature**
```rust
#[get("/audit?<user>&<action>&<from>&<to>&<page>&<page_size>")]
pub async fn list_audit(
    state: &State<AppState>,
    admin: AuthUser,
    user: Option<String>,       // case-insensitive substring on `username`
    action: Option<String>,     // exact match on `action`
    from: Option<String>,       // YYYY-MM-DD or RFC3339 → timestamp $gte
    to: Option<String>,         // YYYY-MM-DD (end-of-day inclusive) or RFC3339 → $lte/$lt
    page: Option<i64>,
    page_size: Option<i64>,
) -> AppResult<Json<AuditPage>>
```

- **Guard:** first line `admin.require_admin()?;` (admin-only; returns 403 for non-admins).
- **Page defaults:** `let page = page.unwrap_or(1).max(1);` `page_size` clamps to one of `[25, 50, 100]`,
  default `50` (audit rows are compact — const `ALLOWED_PAGE_SIZES` + `DEFAULT_PAGE_SIZE`). `skip = (page-1)*page_size`.
- **Filter `$match` document**, built conditionally:
  - `user`: trim; if non-empty → `match_doc.insert("username", doc!{ "$regex": v, "$options": "i" });`
  - `action`: trim; if non-empty → `match_doc.insert("action", v);`
  - `from`/`to`: parse (helper below) into a `timestamp` sub-doc with `$gte` and/or `$lte`/`$lt` using
    **`mongodb::bson::DateTime`** (`BsonDateTime::from_chrono(dt)`), because `write_audit` stores
    `DateTime<Utc>` → BSON date. If both bounds present, merge into one `timestamp` sub-document.
- **Date parsing helper** (module-private):
  ```rust
  // Accepts full RFC3339 (`2026-08-25T13:00:00Z`) or a bare date (`2026-08-25`, treated as 00:00:00 UTC).
  // `end_inclusive` = true is used for `to`: a bare date returns the *start of the next day* so the caller
  // can use `$lt` and include the whole day; a timestamped value returns itself for use with `$lte`.
  fn parse_bound(s: &str, end_inclusive: bool) -> Result<(BsonDateTime, bool /* is_lt */), AppError>
  ```
  Implementation notes: try `DateTime::parse_from_rfc3339` first → `(that, false)`. Else try
  `NaiveDate::parse_from_str(s, "%Y-%m-%d")`: for `from` use midnight UTC `(date, false)`; for `to`
  (`end_inclusive`) use `(date + 1 day at midnight, true)` so it pairs with `$lt`. On parse failure return
  `AppError::BadRequest("invalid date: <s> (use YYYY-MM-DD or RFC3339)")`. Keep it small; `chrono` is already
  a dep. Build the `timestamp` sub-doc accordingly: `from` always `$gte`; `to` inserts `$lt` when `is_lt` else `$lte`.
- **Aggregation pipeline** over `state.db.audit_log()`:
  ```rust
  vec![
      doc!{ "$match": match_doc },
      doc!{ "$sort": { "timestamp": -1 } },          // newest first
      doc!{ "$facet": {
          "rows":  [ { "$skip": skip }, { "$limit": page_size },
                     { "$addFields": { "_id": { "$toString": "$_id" } } } ],
          "total": [ { "$count": "n" } ],
      }},
  ]
  ```
- **Response structs** (both `#[derive(Debug, Serialize)]`, snake_case):
  ```rust
  pub struct AuditRow {
      pub id: String,                    // ObjectId hex (from $toString)
      pub user_id: String,
      pub username: String,
      pub action: String,
      pub resource: String,
      #[serde(skip_serializing_if = "Option::is_none")]
      pub details: Option<Value>,
      pub timestamp: String,             // RFC3339 (see mapping below)
  }
  pub struct AuditPage {
      pub entries: Vec<AuditRow>,
      pub total: i64,
      pub page: i64,
      pub page_size: i64,
      pub page_count: i64,
      pub has_prev: bool,
      pub has_next: bool,
  }
  ```
- **Row mapping** (mirror explorer's `get_array("rows")`/`read_count` handling): for each row doc, read
  `_id`/`user_id`/`username`/`action`/`resource` as strings (`get_str(..).unwrap_or("")`); `details` via
  `doc.get("details")` → `Bson::into_relaxed_extjson()` when present and not `Null`, else `None`; `timestamp`
  read as a BSON datetime (`doc.get_datetime("timestamp")`) and rendered with
  `.to_chrono().to_rfc3339()` — fall back to `""` on absence. `total` via the same `read_count` pattern as
  explorer. `page_count = if total == 0 { 1 } else { (total + page_size - 1) / page_size }`.
  `has_prev = page > 1; has_next = page < page_count`.

### 1b. Module + mount

- `routes/mod.rs`: add `pub mod audit;` (keep the list alphabetical-ish — after `stats`).
- `main.rs`: add a mount block (or extend the explorer block) with `routes::audit::list_audit`.

### 1c. Write-path wiring (complete the trail)

Add best-effort `audit::write_audit(...)` calls at the **successful end** of each handler below, using the
`AuthUser` binding the handler already has. Import is `crate::auth::audit` (connectors/ingest modules will need
the `use`). All these handlers are already admin-guarded and hold `state: &State<AppState>`.

| File / handler | Binding | Call to add (after the successful mutation) |
|---|---|---|
| `auth/routes.rs::create_user` (after `insert_one`, before `Ok`) | `admin` | `audit::write_audit(&state.db, &admin.id, &admin.username, "user_created", &user.id, Some(json!({ "username": user.username, "role": user.role }))).await;` |
| `connectors/routes.rs::create_source` (after `insert_one`) | `user` | `audit::write_audit(&state.db, &user.id, &user.username, "source_created", &source.id, Some(json!({ "name": source.name, "kind": format!("{:?}", source.kind) }))).await;` — read the actual field names/enum from the handler; if `kind` isn't trivially serializable, pass just `{ "name": source.name }`. |
| `connectors/routes.rs::update_source` (after the update succeeds) | `user` | `audit::write_audit(&state.db, &user.id, &user.username, "source_updated", id, None).await;` |
| `connectors/routes.rs::delete_source` (after cascade deletes, before `Ok`) | `user` | `audit::write_audit(&state.db, &user.id, &user.username, "source_deleted", id, None).await;` |
| `ingest/routes.rs::start_ingest` (after the job doc is inserted / spawn kicked off, before returning `job_id`) | read the handler's `AuthUser` binding | `audit::write_audit(&state.db, &<binding>.id, &<binding>.username, "ingest_started", &<source_id>, Some(json!({ "tables": <n> }))).await;` — use the request's source id + selected-tables count already in scope. |

> **Agent must Read each handler first** and adapt the exact field/variable names. Details payloads are
> nice-to-have — if a field isn't readily in scope, pass `None` rather than contort the handler. Never let an
> audit write change a handler's return type or error path (it's fire-and-forget `.await;`).

### Verify (Layer 1)
`cd onprem-rag-server && cargo check` → exit 0 (pre-existing dead-code warnings for `users()`/`Hit.score` etc.
are fine; the `audit_log()` helper stops being dead once `list_audit` uses it). Confirm the 5 new
`write_audit` calls compile and `list_audit` is mounted.

---

## Layer 2 — Bridge (`onprem-rag-app/src-tauri` + `src/lib`)

Mirror the response shape both ways (mirror-hazard: any dropped field disappears silently).

### 2a. `src-tauri/src/commands.rs`
- Add mirror structs next to `RecordsPage` (both `#[derive(Debug, Clone, Serialize, Deserialize)]`, snake_case,
  a mirror-hazard comment):
  ```rust
  pub struct AuditRow {
      pub id: String,
      pub user_id: String,
      pub username: String,
      pub action: String,
      pub resource: String,
      #[serde(skip_serializing_if = "Option::is_none")]
      pub details: Option<serde_json::Value>,
      pub timestamp: String,
  }
  pub struct AuditPage {
      pub entries: Vec<AuditRow>,
      pub total: i64,
      pub page: i64,
      pub page_size: i64,
      pub page_count: i64,
      pub has_prev: bool,
      pub has_next: bool,
  }
  ```
- Add the command, modeled on `list_records` (manual query string, `encode_path_segment` each value, append a
  param only when `Some`/non-empty):
  ```rust
  #[tauri::command]
  pub async fn get_audit(
      user: Option<String>,
      action: Option<String>,
      from: Option<String>,
      to: Option<String>,
      page: i64,
      page_size: i64,
      bridge: State<'_, Bridge>,
  ) -> Result<AuditPage, String>
  ```
  Base `format!("/audit?page={page}&page_size={page_size}")`, then append
  `&user=`/`&action=`/`&from=`/`&to=` (encoded) for each `Some` non-empty value. `bearer_auth(token)`,
  `check_auth(&bridge, &resp)?`, `error_body` on non-success, `resp.json::<AuditPage>()`.
- Register `get_audit` in `src-tauri/src/lib.rs` `invoke_handler![…]`.

### 2b. `src/lib/bridge.ts` + `types.ts`
- In `bridge.ts` (near `RecordsPage`), add exported interfaces mirroring the structs (snake_case fields;
  `details?: unknown` optional; `timestamp: string`):
  ```ts
  export interface AuditRow {
    id: string; user_id: string; username: string; action: string;
    resource: string; details?: unknown; timestamp: string;
  }
  export interface AuditPage {
    entries: AuditRow[]; total: number; page: number; page_size: number;
    page_count: number; has_prev: boolean; has_next: boolean;
  }
  ```
- Add the wrapper (camelCase invoke args; omit-empty by passing `null`):
  ```ts
  export function getAudit(
    filters: { user?: string; action?: string; from?: string; to?: string },
    page: number,
    pageSize: number,
  ): Promise<AuditPage> {
    return invoke<AuditPage>('get_audit', {
      user: filters.user?.trim() || null,
      action: filters.action || null,
      from: filters.from || null,
      to: filters.to || null,
      page,
      pageSize,
    });
  }
  ```
  (Do NOT add anything to `types.ts` unless a shared type is genuinely needed — `AuditRow`/`AuditPage` live in
  `bridge.ts` alongside `RecordsPage`, matching the existing convention.)

### Verify (Layer 2)
`cd onprem-rag-app/src-tauri && cargo check` → exit 0. Grep confirms `AuditPage` mirrored in **both**
`commands.rs` and `bridge.ts`.

---

## Layer 3 — App (`onprem-rag-app/src`)

### 3a. New screen `src/features/audit/index.tsx` (default export, zero-prop)

Admin-only Audit Log. Model the query + pagination + table→cards on `data-explorer/RowsGrid.tsx`.

- **Data:** TanStack `useQuery({ queryKey: ['audit', filters, page, pageSize], queryFn: () => getAudit(filters, page, pageSize), placeholderData: keepPreviousData, staleTime: 30_000 })`.
- **Known-action list** (module const, drives the action `Select`; label-ize for display):
  `login, login_failed, user_created, user_updated, user_deleted, password_set, password_changed, profile_updated, source_created, source_updated, source_deleted, ingest_started`.
  Provide an `ACTION_LABEL` map (e.g. `login_failed → "Login failed"`, `ingest_started → "Ingest started"`) and
  an `ACTION_BADGE` map to a `Badge` variant (failures → `error`/`warning`; deletes → `error`; logins →
  `info`; creates/updates → `success`/`neutral`). Default option: **"All actions"** (empty value).
- **Filter bar** (stacks on mobile, row at `md`): a `user` text `Input` (debounced ~300 ms like RowsGrid's
  search → `debouncedUser`, fed to the query; reset `page` to 1 on change), an action `Select` (options prop),
  two native `date` inputs for **From** / **To** (styled like RowsGrid's page-size `<select>`: `bg-surface
  border border-border rounded-md`), and a **Clear filters** ghost button shown when any filter is set. Any
  filter change resets `page` to 1.
- **Results:**
  - **Mobile (`md:hidden`):** each entry a `Card`-like stacked block — top row `Badge(action)` +
    relative/short time; then `username` (fallback `user_id`), `resource`, and a `details` line rendered as
    compact `key: value` pairs when present (JSON-stringify small objects; never dump huge blobs — truncate).
  - **Desktop (`hidden md:block`):** a real `<table>` inside `overflow-x-auto rounded-lg border border-border`
    (mirror RowsGrid) with columns **Time · Action · User · Resource · Details**. Time = `new
    Date(row.timestamp).toLocaleString()`; Action = `Badge`; Details = truncated stringified JSON with a
    `title` full value.
  - **States:** loading spinner (`Loader2 animate-spin`), error line (`text-danger`), and an `EmptyState`
    (icon e.g. `ClipboardList`/`ScrollText`) with copy that differs when filters are active
    ("No audit entries match these filters.") vs empty ("No audit entries yet.").
- **Pagination footer:** copy RowsGrid's composition — "`start–end of total`" summary + Prev/Next (`ChevronLeft`/
  `ChevronRight`), compact "Page X of Y" on mobile, first/last + page-size `<select>` (`[25,50,100]`) at `md+`.
  Use `data.page_count`, `has_prev`/`has_next` (or derive from page vs page_count as RowsGrid does).
- **Layout/mobile rules:** page wrapper `flex flex-col gap-4` (optionally `max-w-5xl`), tap targets
  `min-h-[44px]`, **zero horizontal body scroll at 360px** (wide table is confined to its own
  `overflow-x-auto` box; the mobile composition is cards). Dark theme tokens only (`text-fg`, `text-fg-muted`,
  `bg-surface`, `border-border`, etc.).
- **Admin gate:** the route is already admin-gated by `permissions.ts`, but add a defensive fallback like
  Profile's `if (!user) …` → if the real role isn't admin, render a short "Administrator access required."
  message (use `useSession(s => s.user?.role) === 'admin'`, **the real role, never `previewRole`**).
- Use only existing UI primitives (`Button`, `Input`, `Select`, `Badge`, `Card`, `EmptyState`, `cn`) and
  `lucide-react` icons. No new deps. No new bridge calls beyond `getAudit`.

### 3b. Route registration — **Opus does this, not the agent**
Add to `src/app/routes.tsx` registry after `/admin`:
```tsx
'/audit':       lazy(() => import('../features/audit'))        as ComponentType,
```

### Verify (Layer 3)
`cd onprem-rag-app && npx tsc --noEmit` → exit 0. Manually reason about 360px (cards, no h-scroll) and 44px
targets. Confirm admin-gating uses the real role.

---

## Definition of done
- `GET /audit` returns newest-first, filtered, paginated `AuditPage`; non-admin → 403; bad date → 400.
- 5 new `write_audit` calls land entries for user-create + source CRUD + ingest-start.
- `AuditPage`/`AuditRow` mirrored in `commands.rs` **and** `bridge.ts`; `get_audit` registered.
- Audit Log screen renders real data, filters + paginates, admin-gated, mobile cards / desktop table,
  no 360px h-scroll.
- `cargo check` (×2) and `npx tsc --noEmit` all exit 0. `routes.tsx` wired by Opus.
- Each layer's diff independently verified by Opus (read the code + re-run the check — not agent self-reports).
