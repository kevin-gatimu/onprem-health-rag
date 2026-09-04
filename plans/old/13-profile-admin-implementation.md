# Stage 9 — Profile + Admin (implementation contract)

**STATUS: IMPLEMENTED (2026-08-25)**

Self-service **Profile** (edit own name, change own password) and admin **User Management**
(list / create / edit / delete / set-password). Ported from the Electron reference
(`pages/Profile/Profile.tsx`, `pages/Admin/Admin.tsx`, `ipc/users.ipc.ts`) but done the correct way
for our Rust-server + Tauri-bridge split. Mobile-first (design at 360px, layer `md:`/`xl:` up).

Workflow: Opus writes this contract + wires `routes.tsx`; Sonnet implements each layer; Opus verifies
every diff (reads the code + re-runs `cargo check` / `tsc`), never trusting self-reports.

---

## Governing reshapes (reference → us)

1. **No `readonly` role.** The reference has 5 roles incl. `readonly`. We have exactly 4:
   `admin | doctor | nurse | analyst`. Role option lists and badge maps drop `readonly` entirely.
2. **No SQLite `session` table.** The reference deletes rows from a `session` table on password
   change / delete to force re-login. We're **stateless JWT** — there is no server session store to
   purge. A changed password simply means the *next* token issuance uses the new hash; existing tokens
   remain valid until they expire (documented, acceptable for on-prem). Do NOT invent a token-revocation
   store in this stage.
3. **No Better-Auth `account` table.** Password hashes live directly on the `User` doc
   (`password_hash`, argon2id). One collection, no join.
4. **Self-service edits own NAME only.** Email + role are admin-assigned (matches reference Profile UX:
   "Contact an admin to change your email", "Assigned by your administrator"). `PATCH /auth/me` accepts
   `name` only.
5. **Email is the unique key.** `users_email_unique` index exists; username has no unique index. Create
   checks username-exists explicitly (already does); PATCH must check email-conflict explicitly.

---

## What already exists (do not rebuild)

- Server `auth/`: `User{id,username,password_hash,role,created_at,email,name,updated_at}`,
  `UserInfo{id,username,role,email,name}`, `Role` enum (4 tiers, `#[serde(alias="user")]` on Doctor),
  `AuthUser` guard + `require_admin()`, `password::{hash_password,verify_password}` (argon2id),
  `audit::write_audit(db,user_id,username,action,resource,details)` (best-effort), `jwt::issue`,
  routes `login` / `me` / `create_user` (POST /auth/users, admin).
- Bridge commands: `login` / `me` / `logout` / `is_authenticated` + `UserInfo` mirror struct.
  **`create_user` is NOT wired in the bridge** — must be added.
- bridge.ts: `login` / `me` / `logout` / `isAuthenticated`; client `User` + `Role` types in `lib/types.ts`.
- App: `stores/session.ts` (`setUser`, `clearUser`), permissions (`/admin`→admin, `/profile`→all),
  nav items for both, UI primitives (`Button`, `Input`, `Select`, `Badge`, `Modal`, `Card`, `EmptyState`).

---

## Layer 1 — Server (`onprem-rag-server`)

### 1a. Add `created_at` to `UserInfo` (`auth/mod.rs`)

`UserInfo` gains `pub created_at: DateTime<Utc>` and `From<&User>` copies `u.created_at`. This flows
through `login` + `me` automatically and backs the Admin "Created" column + Profile "Member since".
**Mirror hazard:** must be added to the bridge `UserInfo` (commands.rs), client `User` (types.ts,
`created_at: string`), and any place constructing a `User` on the client.

### 1b. New routes (`auth/routes.rs`), all under `/` mount

Request/response bodies are explicit below. All list/return shapes use `UserInfo` (never leak
`password_hash`). Every mutation calls `audit::write_audit(...)` best-effort (helper already exists).

```
GET /auth/users                       (admin)  → Json<Vec<UserInfo>>
```
List all users sorted `created_at` ascending (`.sort(doc!{"created_at":1})`).

```
PATCH /auth/users/<id>                (admin)  → Json<UserInfo>
  body UpdateUserRequest { name: Option<String>, email: Option<String>, role: Option<Role> }  (all #[serde(default)])
```
- Load target by `_id`; 404 if absent.
- If `email` present & non-empty & differs: normalize (`trim().to_lowercase()`), check no OTHER user has
  it (`doc!{"email":&new,"_id":{"$ne":&id}}`) → `BadRequest("that email is already in use")`.
- **Last-admin guard:** if target is currently `Admin` and `role` is `Some(non-admin)`, count admins
  (`doc!{"role":"admin"}`); if `<= 1` → `BadRequest("cannot demote the last admin")`.
- Apply only provided fields (name.trim() if non-empty; email normalized; role). Set `updated_at=now`.
  Persist via `$set`. Return fresh `UserInfo`. Audit `"user_updated"`, resource = target id.

```
DELETE /auth/users/<id>               (admin)  → Json<serde_json::Value>  ({ "ok": true })
```
- **Self-delete guard:** `id == admin.id` → `BadRequest("you cannot delete your own account")`.
- Load target; 404 if absent.
- **Last-admin guard:** if target is `Admin` and admin count `<= 1` → `BadRequest("cannot delete the last admin")`.
- `delete_one(doc!{"_id":&id})`. Audit `"user_deleted"`, resource = target id (+username in details).
- (No cascade needed — chat/conversations are scoped by user id and simply become orphaned; do NOT
  delete their data in this stage. Note it.)

```
POST /auth/users/<id>/password        (admin)  → Json<serde_json::Value>  ({ "ok": true })
  body SetPasswordRequest { new_password: String }
```
- 404 if target absent. `new_password` non-empty else `BadRequest`. `hash_password` → `$set password_hash + updated_at`.
  Audit `"password_set"`, resource = target id. (Admin path: no current-password check.)

```
PATCH /auth/me                        (any authed) → Json<UserInfo>
  body UpdateMeRequest { name: String }
```
- `name.trim()` non-empty else `BadRequest("name cannot be empty")`. `$set name + updated_at` on `user.id`.
  Return fresh `UserInfo` (re-read). Audit `"profile_updated"`, resource = user.id.

```
POST /auth/me/password                (any authed) → Json<serde_json::Value>  ({ "ok": true })
  body ChangePasswordRequest { current_password: String, new_password: String }
```
- Load self by `user.id`; `verify_password(current, hash)` false → `Unauthorized` (maps to 401;
  client shows "Current password is incorrect"). `new_password` non-empty else `BadRequest`.
  `hash_password` → `$set`. Audit `"password_changed"`, resource = user.id.

### 1c. Mount (`main.rs`)

Add to the existing auth mount block (the one with `login, me, create_user`):
`list_users, update_user, delete_user, set_user_password, update_me, change_my_password`.

### Layer 1 verification gate (Opus re-runs)
`cargo check` in `onprem-rag-server` exit 0. Read the diff: confirm all guards present, no
`password_hash` in any response, audit calls on every mutation, `created_at` added to `UserInfo`.

---

## Layer 2 — Bridge (`onprem-rag-app/src-tauri` + `src/lib/bridge.ts`)

**Mirror `created_at`** into the commands.rs `UserInfo` struct (`pub created_at: String`) — serde will
carry the RFC3339 string straight through from the server JSON.

New `#[tauri::command]`s in `commands.rs` (all bearer-authed via `bridge.token()`; follow the existing
`me` shape for GET, `create_user` is NEW so model it on `login`'s POST + `me`'s bearer). Ids are UUIDs
(no path-encoding needed):

| Command | Method + path | Args (snake in Rust) | Returns |
|---|---|---|---|
| `create_user` | POST `/auth/users` | `username,password,email?,name?,role?` | `UserInfo` |
| `list_users` | GET `/auth/users` | — | `Vec<UserInfo>` |
| `update_user` | PATCH `/auth/users/<id>` | `id, name?, email?, role?` | `UserInfo` |
| `delete_user` | DELETE `/auth/users/<id>` | `id` | `()` (check success status; ignore body) |
| `set_user_password` | POST `/auth/users/<id>/password` | `id, new_password` | `()` |
| `update_me` | PATCH `/auth/me` | `name` | `UserInfo` |
| `change_my_password` | POST `/auth/me/password` | `current_password, new_password` | `()` |

- Map 401 on `change_my_password` → `Err("current password is incorrect")`; other non-2xx →
  surface the server's `AppError` JSON `message` if present, else `HTTP {status}` (follow the pattern
  the connection/source commands already use for error extraction — read one first).
- Register all 7 in `lib.rs` `invoke_handler`.

bridge.ts (mirror; **invoke args camelCase**):
```ts
export interface UserInfo { id; username; email; name; role: Role; created_at: string }  // extend existing User usage
export function listUsers(): Promise<User[]>
export function createUser(input: { username; password; email?; name?; role?: Role }): Promise<User>
export function updateUser(id, patch: { name?; email?; role?: Role }): Promise<User>
export function deleteUser(id): Promise<void>
export function setUserPassword(id, newPassword): Promise<void>            // invoke arg: newPassword
export function updateMe(name): Promise<User>
export function changeMyPassword(currentPassword, newPassword): Promise<void>
```
Also extend the client `User` type in `lib/types.ts` with `created_at: string`.

### Layer 2 verification gate (Opus re-runs)
`cargo check` exit 0; `npx tsc --noEmit` exit 0. Grep-confirm `created_at` present in BOTH commands.rs
`UserInfo` and bridge.ts/types.ts (mirror hazard). Confirm all 7 commands registered in lib.rs.

---

## Layer 3 — App (`onprem-rag-app/src`)

Dark theme only; every mutating control gated on the REAL role (`useSession(s=>s.user?.role)==='admin'`),
never `previewRole`. Reuse existing `Button/Input/Select/Badge/Modal/Card/EmptyState`. Use TanStack Query.

Shared role display maps (define locally, or a tiny `features/admin/roles.ts` shared by both screens):
```
ROLE_OPTIONS = [admin, doctor, nurse, analyst]           // NO readonly
ROLE_BADGE:  admin→error, doctor→info, nurse→success, analyst→warning
ROLE_LABEL:  admin→"System Administrator", doctor→"Doctor", nurse→"Nurse", analyst→"Data Analyst"
```

### 3a. `features/profile/index.tsx` (all roles)
- Reads `useSession(s=>s.user)`. Avatar = initials from `name` (fallback to `username` if name empty).
  `ROLE_LABEL` + `ROLE_BADGE` badge.
- Fields: **Name** (inline edit → `updateMe(name)`; on success `setUser(updated)` so sidebar/header
  refresh; Enter saves, Esc cancels), **Email** (read-only + hint), **Role** (read-only badge + hint),
  **Member since** (`new Date(user.created_at).toLocaleDateString(...)`).
- "Change Password" → Modal (current / new / confirm). Client validation: current required, new ≥ 8
  chars, new === confirm. Calls `changeMyPassword(current,new)`; inline error on failure (esp. the 401
  "current password is incorrect"). Success toast + close.
- Mobile-first: single column, card `p-4`, ≥44px tap targets, name-edit row wraps at 360px.

### 3b. `features/admin/index.tsx` (admin only — but page is behind the route guard already)
- `useQuery(['users'], listUsers)`.
- Header + "New User" button (`UserPlus`).
- **List — mobile-first table→cards:** below `md` render each user as a stacked label-value **card**
  (Name, Email, Role badge, Created); at `md+` a `<table>`/grid with columns Name · Email · Role ·
  Created · Actions. NO horizontal body scroll at 360px.
- Row actions (admin): **Edit** (Pencil), **Change password** (KeyRound), **Delete** (Trash2). Delete is
  **disabled for the current user's own row** (`u.id === currentUser.id`) — server also enforces, this is
  UX. Highlight the self row subtly.
- **Create modal:** name, email, password, role Select. On submit `createUser({username:<derive>,...})`.
  ⚠️ Server `create_user` requires `username` (not just email). The reference form has no username field.
  **Decision:** add a **Username** field to the create form (required) alongside Name/Email/Password/Role —
  this is the correct way given our schema (username is the login handle + only non-unique-indexed key).
  Order: Username, Full name, Email, Password, Role.
- **Edit modal:** Full name, Email, Role (no password here). `updateUser(id,{name,email,role})`.
- **Change-password modal:** single new-password field → `setUserPassword(id,newPw)` (admin path, no
  current-pw). Client: new ≥ 8 chars.
- **Delete confirm modal:** danger confirm → `deleteUser(id)`. Surface server errors as toasts
  (last-admin / self-delete guards return `BadRequest` text).
- All mutations `invalidateQueries(['users'])` on success + toast. Per-action loading flags.
- Mobile-first: modals use the existing `Modal` (already has the bottom-sheet variant from Stage 0/6).

### 3c. Routes — **Opus wires these** (agents do NOT edit `routes.tsx`)
Add to `app/routes.tsx` registry:
`'/profile': lazy(() => import('../features/profile'))`, `'/admin': lazy(() => import('../features/admin'))`.

### Layer 3 verification gate (Opus re-runs)
`npx tsc --noEmit` exit 0. Read every new file: confirm real-role gating (not previewRole), no
`readonly` anywhere, self-row delete disabled, password confirm/min-8 client checks, `setUser` on
profile name save, 360px composition (cards below md).

---

## Deliberate deviations (record in progress memory when DONE)
1. No token revocation on password-change/delete — stateless JWT; existing tokens live to expiry.
2. Self-service edits **name only** (email/role admin-assigned) — matches reference UX, enforced server-side.
3. **Username field added** to the admin Create form (reference omitted it; our schema needs it).
4. `created_at` added to `UserInfo` to back "Created"/"Member since" (reference read it from SQLite).
5. Deleting a user does **not** cascade-delete their conversations (orphaned, harmless; no cascade route).
6. Audit writes added now for user CRUD + profile (Stage 10 adds the read side `GET /audit`); the
   `write_audit` helper already exists, so wiring the write points here is free and correct.
