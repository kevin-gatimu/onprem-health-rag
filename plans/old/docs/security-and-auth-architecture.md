# Security & Auth Architecture

> **Superseded.** This file is retained for link/history stability. The authoritative replacement is [Security, Auth, and Audit](security-auth-and-audit.md). Do not treat the statuses, defaults, or diagrams below as current.

> How authentication, authorization, session lifecycle, and data protection work in the on-premises
> health-records RAG system — as **currently implemented**, with a clearly marked list of what is still
> planned. This is the engineer-facing companion to the design note `plans/16-security-hardening.md`
> (threat model + full remediation roadmap). Start with [Architecture Overview](architecture-overview.md)
> for the system topology.

**Guiding principle (PHI system):** the **server is the only thing that must never be compromised.** Clients
hold a bearer token and a local cache; the server holds every patient record, every source-DB credential, and
the JWT signing key. Blast radius is asymmetric, so the server gets the strictest controls.

**Status legend:** ✅ implemented · ⚠️ partial / dev-only · ❌ planned (see `plans/16`)

---

## 1. Auth model at a glance

| Property | Choice |
| --- | --- |
| Scheme | **Stateless JWT bearer tokens** (HS256), with **near-stateless revocation** via a per-user token-version |
| Signing | HMAC-SHA256 over `ONPREM_JWT_SECRET` (`jsonwebtoken` crate) |
| Password storage | **argon2id**, per-password random salt, PHC-string format (`argon2` crate) |
| Authorization | **RBAC**, 4 roles baked into the token claims: `admin · doctor · nurse · analyst` |
| Token lifetime | `ONPREM_JWT_TTL_HOURS`, default **8h**; killable early via token-version |
| Core invariant | **The JWT never enters the web layer** — it lives only in the Rust bridge's managed state |
| Enforcement points | (1) DocumentDB credentials, (2) server JWT + Rocket request guard, (3) UI auth gate |

The design is deliberately **stateless-first for performance** (no server-side session table) but adds a single
indexed revocation counter so a stolen/aged token can actually be killed — the one thing a pure stateless JWT
cannot do. See §5.

---

## 2. Trust boundaries & traffic flow

```
React (web layer)  ──invoke──▶  src-tauri bridge  ──reqwest+Bearer──▶  Rocket server  ──▶  DocumentDB
   no token, no                 holds JWT + base                        verifies JWT,        Foundry Local
   direct HTTP                  URL in Rust state                       enforces RBAC        source DBs
```

| Boundary | State | Notes |
| --- | --- | --- |
| React → Rust bridge | ✅ **Strong** | JWT lives only in Rust managed state (`src-tauri/src/state.rs`), never in JS. **Preserve this invariant.** |
| Bridge → server | ⚠️ Weak on LAN | Plaintext HTTP today; token replayable if sniffed. TLS-in-front is the fix (§10). |
| Server → DocumentDB | ⚠️ Dev-permissive | Default URI has `tlsAllowInvalidCertificates=true` (MITM-able); pin the CA in production. |
| Server → source DBs | ✅ Creds encrypted at rest | AES-256-GCM (§8); connection TLS depends on the source. |
| Token at rest (disk) | ❌ Plaintext JSON | `bridge-store.json` on desktop + Android; OS-secure-store swap is planned (§7). |

---

## 3. Identity: the user model

Users are documents in the DocumentDB `users` collection (`onprem-rag-server/src/auth/mod.rs`):

| Field | Purpose |
| --- | --- |
| `_id` (`id`) | UUIDv7 string (time-ordered) |
| `username` | Login identifier |
| `password_hash` | argon2id PHC string — never leaves the server |
| `role` | `Role` enum, serialized lowercase |
| `email` | Unique (indexed); backfilled from `username` on boot for legacy docs |
| `name` | Display name |
| `created_at` / `updated_at` | Timestamps |
| `token_version` | **Session-revocation counter** (§5); `#[serde(default)]` → `0` for legacy docs |

Only a **public projection** (`UserInfo` — id, username, role, email, name, created_at; **no hash**) is ever
returned to clients.

**Roles** (`Role` enum): `Admin`, `Doctor`, `Nurse`, `Analyst`. Legacy documents that stored `"user"`
deserialize transparently as `Doctor` via `#[serde(alias = "user")]` — no migration pass needed.

**Admin seed** (`auth/seed.rs`): on first boot with a reachable DB, a default admin is seeded from
`ONPREM_ADMIN_USERNAME` / `ONPREM_ADMIN_PASSWORD` (defaults `admin` / `password` — see the fail-fast guard in
§9). Idempotent: skipped if the admin username already exists.

---

## 4. Password security

`onprem-rag-server/src/auth/password.rs`:

- **argon2id** with a fresh random salt per password; stored as a PHC string. ✅
- `verify_password` returns `false` on any mismatch or malformed hash and never leaks the reason. ✅
- **Timing-based user-enumeration resistance:** the login path always runs an argon2 verify. On the
  *no-such-user* branch it verifies the supplied password against a fixed `DUMMY_HASH` (`dummy_verify`), so
  "no such user" costs the same as "wrong password" and returns the identical `Unauthorized`. ✅
- ❌ **No password-strength policy** yet — only non-empty is enforced (P2 in `plans/16`).

---

## 5. Sessions, logout & revocation

The mechanism that satisfies *"log users out when the token expires or is revoked."*

### 5a. JWT issue/verify — `auth/jwt.rs`

Claims: `sub` (user id), `username`, `role`, **`tv`** (token-version), `iat`, `exp`. Signed HS256 over
`ONPREM_JWT_SECRET`; expires `ONPREM_JWT_TTL_HOURS` (default **8h**) from issue. `verify()` checks signature +
expiry with `Validation::default()`.

> ❌ **Not yet added:** `iss` / `aud` / `jti` claims (P2 in `plans/16`).

### 5b. Token-version revocation — `auth/mod.rs`, `auth/guard.rs` ✅

Each user carries an integer `token_version`. Every issued token embeds the value **current at issue time** as
its `tv` claim. The `AuthUser` request guard, after verifying the signature, does **one indexed `_id` lookup**
and requires `stored token_version == claims.tv`; any mismatch — or a missing user (deleted account) — is a
**401**. A genuine DB error is a 500.

This keeps the stateless-JWT performance story (a single cacheable indexed field, not a per-request blacklist)
while giving **true revocation**: bumping `token_version` invalidates every token issued before the bump.

**`token_version` is bumped (`$inc`) on** (`auth/routes.rs`):

| Trigger | Route | Effect |
| --- | --- | --- |
| User logs out | `POST /auth/logout` | Kills the caller's own outstanding tokens |
| Admin force-logout | `POST /auth/users/<id>/logout` (admin only) | Kills a target user's tokens ("log out everywhere") |
| Self password change | `POST /auth/me/password` | All the user's sessions die; re-login with the new password |
| Admin password reset | `POST /auth/users/<id>/password` | Target's sessions die (e.g. compromised account) |
| Role change | `PATCH /auth/users/<id>` (when role actually changes) | New role takes effect **immediately**, not at token expiry |

So expiry, admin revocation, and security-state changes all converge on the same reject-at-the-guard path.

### 5c. Request guard & RBAC — `auth/guard.rs`

`AuthUser` is a Rocket `FromRequest` guard. Naming it as a route parameter makes the route require a valid
token. It: extracts the `Bearer` token (case-insensitive), calls `jwt::verify`, runs the token-version check
(§5b), and exposes `{ id, username, role }`. `require_admin()` returns `Forbidden` for non-admins and gates
every admin route (user CRUD, force-logout, model/pull, etc.).

### 5d. Login throttle — `auth/throttle.rs` ✅

In-memory **per-IP** exponential backoff on `/auth/login`: after `5` failures the IP is locked, the lock window
doubles per further failure from `30s` up to a `15min` cap, and records reset after `15min` idle. A locked IP
gets `429 Too Many Requests` with a retry hint. Process-local (sufficient for the single-host on-prem
deployment; a cluster would move this to shared state). Every failure is audited (§8).

---

## 6. Client-side session handling (bridge + React)

The critical invariant: **the token never touches JavaScript.** The React layer only ever sees *authenticated
vs. not*; the bridge holds the actual token.

- **Bridge** (`src-tauri/src/commands.rs`, `state.rs`): base URL + JWT live in a Rust `Mutex`; `reqwest` attaches
  the `Bearer` header. The token is persisted to `bridge-store.json` via `tauri-plugin-store` so it survives an
  app restart (⚠️ plaintext today — see §7).
- **401 → forced logout (centralized):** the bridge maps any 401 from an authenticated call to a typed sentinel
  `__SESSION_EXPIRED__` (and clears its own token). On the frontend, every authenticated wrapper goes through
  `authedInvoke` (`src/lib/bridge.ts`); on the sentinel it fires a registered handler and rethrows a
  human-readable error (the raw sentinel never escapes). ✅
- **The handler** (`src/main.tsx` → `setSessionExpiredHandler`) calls `useSession.getState().clearUser()`,
  flipping `authStatus` to `unauthenticated` so the top-level gate in `App.tsx` re-renders straight to Login,
  and shows **one** toast. Concurrent 401s dedupe naturally: `clearUser()` flips the status before the next
  handler runs, so only the first (while still authenticated) toasts. ✅
- **Explicit logout button:** present in **every layout** — the desktop `Sidebar` footer (icon in the rail /
  collapsed states, icon + label when expanded) and the mobile `NavSheet` footer. It calls the async `logout`
  bridge command (best-effort `POST /auth/logout` to bump `token_version` server-side, then clears the local
  token + store) followed by `clearUser()` in a `finally`, so the UI always returns to Login even if the server
  is unreachable. ✅
- **Deliberate exception:** `change_my_password` maps its 401 to *"current password is incorrect"* — that 401
  means wrong password, **not** a dead session, so it must not force-logout.
- ❌ **Proactive expiry timer** (flip to Login at `exp` without waiting for the next call) is intentionally
  deferred — the reactive path fully covers expiry, and exposing `exp` would add surface to the web layer.

---

## 7. Token at rest (desktop & Android)

⚠️/❌ Today the token + server URL are persisted in **plaintext JSON** (`bridge-store.json`). On a lost or
rooted device — especially Android — that yields a live token until it expires or is revoked. The short TTL
(§5a) + revocation (§5b) bound the blast radius, but the planned fix is to move the token into the **OS secure
store** (Windows Credential Manager / Android Keystore-backed `EncryptedSharedPreferences`, e.g. the `keyring`
crate or a Tauri secure-storage plugin). Tracked as **P0 (mobile)** in `plans/16`; deferred to on-device work.

---

## 8. Data protection

### Source-DB credential encryption — `crypto.rs` ✅
Source-database passwords are encrypted at rest with **AES-256-GCM** (random 12-byte nonce per value; stored as
`base64(nonce ‖ ciphertext+tag)`, self-describing so no separate nonce column). The 256-bit key is derived
`SHA256("onprem-rag/credentials/v1:" ‖ secret)` where `secret` is `ONPREM_CREDENTIALS_KEY` (⚠️ falls back to the
JWT secret in dev, with a warning; a distinct key is required in production — §9). ❌ Switching derivation to
HKDF-SHA256 is a planned hardening (P1).

### Audit log — `auth/audit.rs` ✅
Security-relevant events are appended to the `audit_log` collection as `AuditEntry`
`{ user_id, username, action, resource, details?, timestamp }`. Writes are **best-effort** — a failure warns and
never breaks the user's request. Captured actions include `login`, `login_failed`, `logout`, `force_logout`,
`user_created` / `user_updated` / `user_deleted`, `password_set`, `password_changed`, `profile_updated`.
Read side: `GET /audit` (admin). Treat the log as append-only; define retention operationally (§10).

---

## 9. Fail-fast configuration — `config.rs` + `main.rs` ✅

All config comes from the environment (prefix `ONPREM_`; `.env` in dev via `dotenvy`). `Config::security_issues()`
collects insecure settings; at boot `main.rs` **panics (refuses to start)** when `ONPREM_ENV=production` and any
issue is present, and merely warns in development. Checks:

- Weak/default `ONPREM_JWT_SECRET` (the dev placeholder, or `< 32` chars).
- Default/empty `ONPREM_ADMIN_PASSWORD` (`"password"`).
- `ONPREM_CREDENTIALS_KEY` **missing**, `< 32` chars, or **equal to** `ONPREM_JWT_SECRET` (two secrets, two
  jobs — sharing them halves the security).

### Security-relevant configuration keys

| Var | Meaning | Production requirement |
| --- | --- | --- |
| `ONPREM_ENV` | `production`/`prod` flips warnings into a hard boot failure | Set to `production` |
| `ONPREM_JWT_SECRET` | HS256 signing key | Independent, ≥32-byte random |
| `ONPREM_JWT_TTL_HOURS` | Access-token lifetime | Default 8; shorter is safer |
| `ONPREM_CREDENTIALS_KEY` | AES key material for source creds | Independent, ≥32-byte random, **≠ JWT secret** |
| `ONPREM_ADMIN_USERNAME` / `ONPREM_ADMIN_PASSWORD` | First-boot admin seed | Change the password off the default |
| `ONPREM_DOCUMENTDB_URI` | DB connection | Drop `tlsAllowInvalidCertificates`, pin CA |

> Rotating `ONPREM_JWT_SECRET` logs everyone out (acceptable). Rotating `ONPREM_CREDENTIALS_KEY` requires
> re-encrypting every stored source credential.

---

## 10. Transport & deployment (operational)

- ⚠️ The server binds **plaintext HTTP** on `0.0.0.0:8000`; the Android manifest currently permits cleartext
  **globally**. Acceptable only on a trusted, segmented LAN.
- ❌ **TLS-in-front:** terminate TLS at a reverse proxy (Caddy/nginx) with a real hostname + certificate for
  anything beyond a trusted LAN; then scope the Android `network_security_config.xml` to that one host (and
  optionally pin its cert). Steps are in `onprem-rag-app/README.md` → *Securing the phone connection*.
- ❌ **DocumentDB link:** pin the container CA and drop `tlsAllowInvalidCertificates` in production.
- **Non-negotiables:** least-privileged (read-only) source-DB accounts for ingestion; network segmentation so
  only the client subnet (or the proxy) reaches `:8000`; encrypted PHI backups; `cargo audit` + `npm audit` in
  CI; audit-log retention policy.

---

## 11. Implemented vs. planned (audit scorecard)

Full findings, severities, and file-level remediation are in `plans/16-security-hardening.md`. Summary:

**✅ Implemented**
- argon2id hashing + constant-time missing-user path (enumeration-resistant login)
- Per-IP login rate-limit / lockout with exponential backoff
- Token-version **revocation** + short TTL; `/auth/logout` + admin force-logout; bumps on password/role change
- Centralized 401 → forced-logout in React; visible logout button in every layout
- AES-256-GCM encryption of source-DB credentials at rest
- Audit log of auth + CRUD + ingest events
- Fail-fast production config validation (refuse to boot on weak/shared secrets)
- **JWT never enters the web layer** (core architectural control) — plus the aggregation operator allowlist
  (`aggregation/validate.rs`) blocking arbitrary JS in aggregation, and parameterized `sqlx` queries

**❌ Planned / open** (with `plans/16` severity)
- **P0:** `.env` not gitignored (holds all secrets) · client token in plaintext JSON → OS secure store
- **P0 (mobile) / P1 (LAN):** cleartext HTTP → TLS-in-front + scoped Android cleartext
- **P1:** 5xx responses leak internal/DB error text (`error.rs`) · strict webview CSP + confirm markdown blocks
  raw HTML/unsafe URLs · connector table/column identifier allowlist + `$prefix` regex escaping ·
  request-size limits + per-user LLM concurrency caps · HKDF for the credential key · drop
  `tlsAllowInvalidCertificates` in prod
- **P2:** password-strength policy · `iss`/`aud`/`jti` claims · minimize Tauri capabilities

---

## Related docs

- **[Architecture Overview](architecture-overview.md)** — components, traffic flow, data model, module structure.
- **`plans/16-security-hardening.md`** — the full threat model, audit findings by severity, and remediation
  roadmap this doc tracks against.
- Persistent memory: `[[security-hardening-plan]]`, `[[bridge-mirror-structs-drop-fields]]`.

---

Source: synthesized from `plans/16-security-hardening.md` and the implemented `onprem-rag-server/src/auth/`,
`crypto.rs`, `config.rs`, and `onprem-rag-app` bridge/frontend. Last updated: 2026-08-26 (token-version
logout + centralized forced-logout shipped).
