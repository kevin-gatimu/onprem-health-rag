# 16 — Security hardening: server, desktop, Android, and session/logout

**STATUS: PROPOSED (2026-08-25)** — design note, not yet implemented. Awaiting review before any code lands.

This is the security design for the whole system, written after an audit of the current auth/config/crypto/
error/bridge/manifest surface. It covers what the user asked for: **hardening the API/server (emphasized),
the desktop app, the Android build, and a real "log the user out when the token expires" mechanism.** It is
organized as: threat model → findings (with severity) → the session/logout design (the explicit ask) →
prioritized remediation roadmap → operational non-negotiables.

The guiding principle for a PHI system: **the server is the only thing that must never be compromised.** The
clients hold a bearer token and cache; the server holds every patient record, every source-DB credential,
and the signing key. Blast radius is asymmetric, so the server gets the strictest controls and the shortest
trust assumptions.

---

## 1. Threat model & trust boundaries

**Assets, most-valuable first:** (1) the JWT signing secret — forges any identity; (2) `ONPREM_CREDENTIALS_KEY`
— decrypts every stored source-DB password; (3) the DocumentDB contents — all ingested PHI + embeddings;
(4) source-DB credentials; (5) a live user's bearer token.

**Adversaries we design against:**

- **LAN attacker / passive sniffer** — the server speaks plaintext HTTP on `0.0.0.0:8000`; anyone on the
  segment can read tokens, PHI, and (at login) credentials off the wire.

- **Stolen / rooted device** — especially Android: token + server URL are persisted to a plaintext JSON
  file on disk; a lost phone or a rooted attacker reads it.

- **Online brute-forcer** — no rate limit or lockout on `/auth/login` today.
- **Malicious / curious insider** — a valid low-privilege user (nurse/analyst) probing for RBAC gaps or
  trying to read data outside their role.

- **Compromised admin token** — an admin can define source connections with arbitrary SQL; a stolen admin
  token pivots to the source databases.

- **Webview content injection (stored XSS)** — chat/agent answers render LLM output (derived from PHI) as
  markdown inside a Tauri webview that currently has **no CSP**.

**Trust boundaries (and how well they hold today):**

| Boundary | Current state |
| --- | --- |
| React web layer → Rust bridge | **Good** — JWT lives only in Rust managed state, never in JS. Preserve this invariant. |
| Bridge → server | **Weak** — plaintext HTTP; token replayable if sniffed. |
| Server → DocumentDB | **Weak** — `tlsAllowInvalidCertificates=true` (MITM-able). |
| Server → source DBs | Creds encrypted at rest (AES-256-GCM); connection TLS depends on the source. |
| Token at rest (disk) | **Weak** — plaintext JSON on both desktop and Android. |

---

## 2. Findings (audit, 2026-08-25)

Severity: **P0** = fix before this ever touches real PHI; **P1** = hardening for any networked deployment;
**P2** = defense-in-depth. File references are the exact sites to change.

### Secrets & configuration

- **[P0] `.env` is not gitignored.** `git check-ignore .env` → not ignored; it shows as untracked in
  `git status`. It holds `jwt_secret`, `admin_password`, `credentials_key`. One `git add .` leaks every
  secret into history. → Add `.env` (and `**/.env`, `bridge-store.json`) to a root `.gitignore` now.

- **[P0] Insecure defaults with no fail-fast** (`config.rs`): `jwt_secret="dev-only-change-me"`,
  `admin_password="password"`, `credentials_key` optional (falls back to the JWT secret). Ship unchanged and
  anyone can forge an admin token *and* decrypt stored source credentials. → The server must **refuse to
  boot** when `ONPREM_ENV=production` (new var) and any secret is default/weak/short.

- **[P1] Credential key derivation** (`crypto.rs`): single `SHA256(domain ‖ secret)`, and it falls back to
  the JWT secret when `credentials_key` is unset. Fine if the key is high-entropy and distinct; risky as a
  passphrase or when shared with JWT signing. → Require a distinct ≥32-byte `credentials_key` in production
  (fail-fast); switch derivation to HKDF-SHA256. Two secrets, two jobs.

### Authentication & session

- **[P0] No login rate-limiting or lockout** (`auth/routes.rs`). Unbounded password guessing. → Per-IP +
  per-account throttle with exponential backoff and temporary lockout; failures are already audited.

- **[P1] Timing-based user enumeration** (`auth/routes.rs`): a missing user returns `Unauthorized` *without*
  running argon2, so "no such user" is measurably faster than "wrong password." → Always run a dummy argon2
  verify against a fixed hash on the missing-user path; return the identical error either way.

- **[P1] No revocation / no server-side logout** (`jwt.rs`, `guard.rs`). The token is fully stateless with
  the role **baked into the claims**: a demoted or deleted user stays valid until `exp` (up to 12h), a stolen
  token can't be killed, and "log out everywhere" is impossible. → Token-version design in §3.

- **[P1] Client expiry handling is reactive and incomplete** (`commands.rs`, `session.ts`). On a 401 the
  bridge clears its own token and returns "session expired", but the React `authStatus` stays
  `"authenticated"` — a half-logged-in limbo that only surfaces as a per-call toast. `clearUser()` is wired
  **only** to the manual logout buttons (`NavSheet.tsx`, `Sidebar.tsx`). There is **no centralized
  401→forced-logout**. → This is the user's explicit ask; design in §3.

- **[P2] No password strength policy** — only non-empty is enforced. **[P2] JWT has no `aud`/`iss`/`jti`.**
  **[P2] 12h TTL is long** for a bearer token with no revocation.

### Transport & server hardening

- **[P0 mobile / P1 LAN] Cleartext HTTP everywhere.** Server binds plain HTTP; the Android manifest forces
  `usesCleartextTraffic="true"` **globally**. On a hostile/shared LAN, tokens + PHI + login credentials are
  readable. → TLS is the single biggest "never be compromised" lever (see §4/§5).

- **[P1] 500-class responses leak internals** (`error.rs`): `respond_to` logs detail server-side (good) but
  the client body is `self.to_string()`, so `Internal(String)` and `Database(e)` ship raw internal/DB error
  text to the caller. → Generic client message for 5xx; keep detail in the server log only.

- **[P1] No request-size limits or per-user concurrency caps** (`main.rs`). `/chat`, `/search`, `/ingest`,
  `/schema/analyze` each drive the local LLM — cheap to trigger, expensive to serve → resource-exhaustion
  DoS. → Rocket body limits + a per-user in-flight cap on LLM endpoints.

- **[P1] DocumentDB connection is MITM-able** (`config.rs`): default URI has
  `tlsAllowInvalidCertificates=true`. → In production, pin the container CA and drop the flag; document.

- **[P2] No security headers / not applicable-but-note.** No CORS by design (native `reqwest`, not a
  browser). If a browser SPA is ever pointed at the server, revisit.

### Injection & data

- **[P1] SQL identifier interpolation in connectors** (`connectors/mod.rs`): `format!("SELECT * FROM {t}")`
  and the MySQL/MSSQL quoted variants interpolate the **table name** directly. It's admin-supplied today, but
  an admin ≠ the source DBA, and a compromised admin token (see brute-force) pivots to arbitrary SQL. The
  custom `query` field is arbitrary SQL by design. → Allowlist table/column identifiers against the
  introspected schema (only names `get_schema` actually returned); quote per dialect; keep custom `query`
  admin-only and document the read-only-source-account guidance.

- **[P1] Regex injection in `$text`/prefix path.** `$prefix` builds `format!("^{prefix}")` unescaped → a
  crafted prefix becomes a match-all or a ReDoS. → Escape regex metacharacters in user-supplied prefixes.

- **[Keep — good control] Aggregation operator allowlist** (`aggregation/validate.rs`): `$where`,
  `$function`, `$accumulator`, `$expr` are blocked, so no arbitrary JS in aggregation. Preserve and test.

### Desktop & Android client

- **[P0 mobile] Token + server URL persisted in plaintext** (`commands.rs` → `bridge-store.json`). A lost or
  rooted device yields a live token. → Store the token in the OS secure store — Windows Credential Manager /
  Android Keystore-backed `EncryptedSharedPreferences` (via `keyring` crate or a Tauri secure-storage
  plugin), not a plaintext JSON file. Short TTL + revocation (§3) bounds the blast radius meanwhile.

- **[P1] `store:default` capability may expose the token to JS** (`capabilities/default.json`). The store
  plugin is reachable from the web layer; if the token key sits in that same store, JS can read it —
  defeating "JWT never in the web layer." → Verify, and either scope the capability so the web layer cannot
  read the token store or move the token entirely out of `tauri-plugin-store`.

- **[P1] `csp: null`** (`tauri.conf.json`). No Content-Security-Policy on a webview that renders
  PHI-derived LLM markdown. → Set a strict CSP; confirm `react-markdown` disallows raw HTML and
  `javascript:`/`data:` URLs (no `rehype-raw`).

- **[P2] Capabilities not minimized** — audit `core:default`/`opener:default` down to what's used.

---

## 3. Session lifecycle & logout (the explicit ask)

**Goal:** users are logged out cleanly when their token expires, when an admin revokes them, and when their
own security state changes (password change, role change) — without turning the server stateful-and-slow.

### 3a. Server: token versioning (real revocation, still ~stateless)

Add an integer `token_version` to each user doc (default 0). Embed it in the JWT as a `tv` claim at login.
The `AuthUser` guard, which already loads context per request, compares `claims.tv` against the user's
current `token_version`; **mismatch → 401.** Bump `token_version` whenever we must invalidate:

- password change (self or admin-set),
- role change (fixes the "demoted user valid until exp" gap immediately),
- account disable/delete,
- explicit **"log out everywhere."**

This is one indexed field (cacheable), not a per-request DB blacklist, so it keeps the stateless-JWT
performance story while giving true revocation. Also shorten the access-token TTL to **~1h** and add
`iss`/`aud`.

New/changed endpoints:

- `POST /auth/logout` — bumps the caller's own `token_version` (or, if we keep long sessions, just a
  client-side drop; see 3c). Audited.

- `POST /auth/users/<id>/logout` — admin force-logout of one user (bumps their `tv`).
- Password-change routes must bump `tv` (they don't today).

*Optional (decide at review):* a refresh-token model — short access token (~15m) + longer refresh token
(revocable, `tv`-checked, stored only in the OS secure store) — for a smoother UX on mobile where re-login
is friction. Simpler alternative: single ~1h token + silent re-login prompt. **Recommendation:** start with
the single short-TTL token + `token_version` revocation (small, high-value); add refresh only if the 1h
re-login proves annoying on Android.

### 3b. Bridge: already most of the way there

`commands.rs` already clears its in-memory + on-disk token on 401 for `check_auth`/`me`/`get_stats`. Extend
that to **every** authenticated command via one helper (map any 401 to a single typed error, e.g.
`"__SESSION_EXPIRED__"`, and clear the token once). Keep the deliberate exception: `change_my_password` must
**not** clear on 401 (that 401 means "current password wrong", not "session dead").

### 3c. React: the missing centralized forced-logout

This is the actual gap behind the user's request. Add one interceptor so expiry anywhere ends the session
everywhere:

1. **Single invoke wrapper** (`lib/bridge.ts`): every bridge call goes through one function that, on the
   typed session-expired error, calls `useSession.getState().clearUser()`, sets
   `authStatus = "unauthenticated"`, toasts "Session expired — please sign in" **once** (dedupe so a burst of
   in-flight calls doesn't stack toasts), and lets the existing `RequireAuth` gate route to Login.

2. **Proactive expiry** — the bridge knows `exp`; expose remaining lifetime (not the token) so the app can
   pre-emptively flip to unauthenticated at expiry instead of waiting for the next failed call. A lightweight
   timer/`setTimeout(exp - now)` is enough; no polling.

3. **Result:** token expiry, admin force-logout, and password/role change all converge on the same clean
   path — bridge returns session-expired → web layer clears user → Login screen. No half-logged-in limbo.

---

## 4. Prioritized remediation roadmap

**P0 — before any real PHI (small, high-leverage):**

1. `.gitignore` `.env`, `**/.env`, `bridge-store.json`. Rotate any secret ever committed. *(minutes)*
2. Fail-fast config validation: with `ONPREM_ENV=production`, refuse to boot on default/weak/short
   `jwt_secret`, default `admin_password`, or missing/short `credentials_key`. Force first-boot admin
   password change. *(`config.rs`, `main.rs`, `seed.rs`)*

3. Login rate-limit + lockout + constant-time missing-user path. *(`auth/routes.rs`)*
4. Move the client token out of plaintext JSON into the OS secure store. *(`commands.rs`, `state.rs`)*
5. TLS story decided and documented — reverse-proxy (Caddy/nginx) in front for LAN+, and a **scoped**
   Android `network_security_config.xml` that permits cleartext only to the specific on-prem host, not
   globally. *(deploy + `AndroidManifest.xml`)*

**P1 — networked-deployment hardening:**

6. Token versioning + revocation + shorter TTL + `iss`/`aud`; force-logout endpoints. *(`jwt.rs`, `guard.rs`,
   `auth/routes.rs`)* — pairs with §3.

7. Centralized 401→forced-logout in React + proactive expiry. *(`lib/bridge.ts`, `stores/session.ts`)*
8. Generic 5xx client bodies; detail only in logs. *(`error.rs`)*
9. Strict CSP; confirm `react-markdown` blocks raw HTML/unsafe URLs. *(`tauri.conf.json`, chat renderer)*
10. Identifier allowlisting in connectors; escape `$prefix` regex. *(`connectors/mod.rs`, retrieval)*
11. Request-size limits + per-user LLM concurrency cap. *(`main.rs`, `rag/`, `ingest/`)*
12. HKDF for credential key; require distinct `credentials_key`; drop `tlsAllowInvalidCertificates` in prod.
    *(`crypto.rs`, `config.rs`)*

**P2 — defense-in-depth:**

13. Password strength policy. 14. Minimize Tauri capabilities. 15. `jti` + optional short denylist for
individual-token kill. 16. Security headers if a browser client is ever introduced.

---

## 5. Operational non-negotiables (deployment, not code)

- **Secret generation & storage** — generate `jwt_secret` and `credentials_key` as independent ≥32-byte
  random values; never reuse; store outside the repo (env/secret manager). Document rotation (rotating
  `jwt_secret` logs everyone out — acceptable; rotating `credentials_key` requires re-encrypting stored
  source creds).

- **TLS in front** — a reverse proxy terminates TLS for the server; the DocumentDB link uses a pinned CA in
  production. Cleartext only on a trusted, segmented LAN, and even then scoped on Android.

- **Least-privileged source-DB accounts** — ingestion needs read-only; document and recommend read-only DB
  users so a compromised connection can't write to the source.

- **Network segmentation** — the server host (Rocket + Foundry + DocumentDB) sits on a restricted segment;
  only the client subnet reaches `:8000` (or only the proxy does).

- **Audit retention & integrity** — the audit log already captures login/failure/CRUD/ingest; define
  retention and treat it as append-only.

- **Dependency scanning** — `cargo audit` (both crates) and `npm audit` in CI; pin and review.
- **Backups** — DocumentDB backups are PHI; encrypt at rest and control access like the live data.

---

## 6. What's already good (preserve, don't regress)

argon2id password hashing with per-password salt (`password.rs`); AES-256-GCM with a random 12-byte nonce
for source creds (`crypto.rs`); the aggregation operator allowlist blocking arbitrary JS (`validate.rs`);
parameterized `sqlx` everywhere except the admin-only identifier interpolation; and the core architectural
control — **the JWT never enters the web layer.** The hardening above must not weaken any of these; in
particular, moving token storage and adding the invoke wrapper must keep the token out of JS.

---

## Definition of done (when implemented)

- Server refuses to boot with insecure secrets in production; `.env`/token file gitignored.
- Login is rate-limited, locks out, and is constant-time on the missing-user path.
- Tokens are revocable (token-version), short-lived, and killed on password/role change and admin
  force-logout; "log out everywhere" works.

- Expiry anywhere → one clean forced-logout to the Login screen (no half-logged-in state, no toast storms).
- Client token is in the OS secure store, not plaintext JSON; webview has a strict CSP.
- 5xx responses no longer leak internals; connector identifiers are allowlisted; LLM endpoints are bounded.
- TLS path documented; Android cleartext scoped to the on-prem host only.
- `cargo check` (both crates) + `npx tsc --noEmit` clean; each change verified by Opus, not agent self-report.
