# Security, Authentication, and Audit

**Authoritative status:** code-verified on 2026-09-02. Current code overrides older plan status labels.

This document describes the current security boundary for the on-premises health-records RAG system. It separates controls that are implemented from controls that remain partial or planned.

## Security boundary

React never calls the server directly and does not receive the bearer token. Calls flow through the Tauri Rust bridge, which owns the server URL and JWT, adds the `Authorization` header, and relays server SSE as Tauri events.

| Boundary | Current posture |
| --- | --- |
| React → Tauri bridge | **Implemented:** JWT is kept out of normal JavaScript state and bridge response types expose only public user fields. |
| Token at rest | **Open risk:** the bridge persists the JWT and base URL in plaintext `bridge-store.json`. The file is ignored by Git, but is not an OS secure store. |
| Bridge → Rocket server | **Open risk:** the default connection is plain HTTP. A network observer can read credentials, JWTs, and PHI. |
| Server → DocumentDB | **Open risk:** the default URI enables TLS while accepting invalid certificates. It does not authenticate the server certificate. |
| Source credentials at rest | **Implemented with a remaining migration:** AES-256-GCM with a random 96-bit nonce; key material is domain-separated with SHA-256. HKDF and ciphertext/key-version migration are not implemented. |

## Authentication and authorization

### Implemented authentication controls

- Passwords are hashed and verified with Argon2id and per-password random salts.
- A missing login identifier still executes a dummy Argon2 verification, reducing username-enumeration timing differences.
- Login accepts username or email and returns the same unauthorized response for an unknown user and a wrong password.
- A process-local, per-IP login throttle starts locking after five failures. The initial lock is 30 seconds, doubles after additional failures, caps at 15 minutes, and resets after 15 minutes idle. It is suitable for the current single-process deployment, not a future cluster.
- JWTs use HS256 and carry user id, username, role, issue/expiry times, and `tv`, the user's token-version counter. The configured default lifetime is eight hours.
- Every authenticated request verifies the JWT, loads the user, and compares the stored `token_version` with `tv`. Deleted users and version mismatches receive `401`.
- Four roles exist: `admin`, `doctor`, `nurse`, and `analyst`. Administrative handlers call `require_admin`; conversation access is scoped by authenticated user id and returns `404` on ownership mismatch.
- Last-admin and self-delete guards protect user administration. Password hashes are never part of `UserInfo` responses.

### Revocation and logout

`token_version` is incremented on:

- self logout (`POST /auth/logout`);
- administrator force-logout (`POST /auth/users/<id>/logout`);
- self password change;
- administrator password reset; and
- an actual role change.

The bridge performs best-effort server logout, aborts its active client-side streams, removes the persisted token, and clears in-memory state. For authenticated bridge commands, `401` is converted to `__SESSION_EXPIRED__`; the shared frontend wrapper invokes the centralized session-expired handler, clears the user, and returns the app to login. The password-change command deliberately treats its own `401` as an incorrect current password rather than an expired session.

**Partial:** expiry handling is reactive. There is no proactive timer that moves the UI to login exactly at JWT expiry without a subsequent authenticated request.

## Secrets and production startup

### Implemented secret controls

The root [`.gitignore`](../../.gitignore) excludes `.env`, `.env.*` except `.env.example`, and `bridge-store.json`. This corrects the older plan's claim that these files were not ignored.

Configuration is environment-driven. In development, `dotenvy` loads the nearest `.env` found by its normal search behavior, while real environment variables win. In `ONPREM_ENV=production` or `prod`, startup refuses to continue when any of these conditions exists:

- the JWT secret is a known development placeholder or shorter than 32 characters;
- the admin password is empty or still `password`;
- the credentials key is absent or shorter than 32 characters; or
- the credentials key equals the JWT signing secret.

Development logs warnings instead of aborting. Secret rotation remains operational work: changing the JWT secret invalidates all tokens; changing the credential key requires re-encrypting stored source credentials, for which no migration command exists.

## Admission and abuse controls

Implemented server admission controls bound expensive work with configurable semaphores and a finite acquisition deadline:

- active generations;
- active retrieval requests;
- active ingestion jobs globally; and
- active ingestion jobs per source.

Capacity exhaustion returns `429` with a retry hint. The readiness endpoint reports currently available generation, retrieval, and ingestion permits.

These controls are global rather than per-user. Rocket request-body limits beyond framework/default configuration, account-based login lockout, password-strength enforcement, and distributed throttling are not implemented.

## Audit trail

### Implemented write path

Security-relevant events are appended to the `audit_log` collection as `{ user_id, username, action, resource, details?, timestamp }`. Writes are best-effort: failure is logged and does not fail the primary request. Current call sites cover:

- successful and failed login, logout, and administrator force-logout;
- user creation, update, deletion, and administrator password reset;
- profile update and self password change;
- source creation, update, and deletion;
- ingestion start; and
- NL-to-SQL execution-related actions wired by current routes.

### Implemented read path

`GET /audit` is administrator-only, newest-first, and paginated with page sizes 25, 50, or 100. It supports action, username-substring, and UTC date/time bounds. The desktop UI exposes the mirrored page through the Tauri bridge.

### Open audit risks

- Writes are not transactional with the primary mutation and can be lost.
- No tamper-evident chaining, append-only database permission policy, export, archival, or retention policy is implemented.
- Operational trace logs are not an audit substitute and are process-memory only.
- The username filter uses a client-provided MongoDB regular expression without escaping; administrators are trusted, but malformed or expensive patterns remain possible.

## Transport and client hardening status

| Item | Status | Current fact |
| --- | --- | --- |
| Server TLS | **Planned/deployment work** | Rocket serves plain HTTP by default on `0.0.0.0:8000`; use a reverse proxy and a real certificate for production traffic. |
| Android cleartext | **Partial/high risk** | The manifest and base network-security policy permit cleartext globally because the server may be entered as a runtime LAN IP. The file contains commented HTTPS/pinning guidance, not an enforced production restriction. |
| DocumentDB certificate validation | **Open risk** | The default URI contains `tlsAllowInvalidCertificates=true`; production startup validation does not currently reject it. |
| Webview CSP | **Open risk** | `tauri.conf.json` still has `csp: null`. |
| Token secure storage | **Planned** | `bridge-store.json` remains plaintext; `store:default` is also granted to the webview capability. |
| Error sanitization | **Open risk** | Although 5xx errors are logged server-side, `AppError::respond_to` still serializes `self.to_string()`, exposing `Internal` and database details to callers. |
| Prefix matching | **Open risk** | aggregation `$prefix` still builds `^<prefix>` without `regex::escape`; regex metacharacters are active. |
| Credential KDF | **Planned** | SHA-256 domain separation remains; no HKDF migration/versioned decrypt path exists. |
| JWT claims | **Planned defense-in-depth** | No issuer, audience, or JWT id claims are enforced. |
| Tauri capabilities | **Partial** | Broad default core/opener/store capabilities remain and need least-privilege review. |

The Android production posture must not be described as scoped cleartext: the current `base-config` permits it for every destination. Conversely, `.env` and bridge-store ignore rules, production secret fail-fast, login throttling, dummy verification, revocation, centralized `401` handling, admission controls, and audit are already implemented and must not be listed as future work.

## Planned data-minimization rules for schema binding and agents

**Status: Planned — see [plans/new/01](../new/01-service-line-ontology-and-schema-binding.md), [04](../new/04-structured-execution-and-fallbacks.md), and [05](../new/05-hospital-agents-server.md).** These rules are design invariants for the service-line binding and executor workstream; they are not yet enforced by code.

- **No PII in `enum_values`.** The binder collects distinct values only for `Status | Category | Type | Severity | Priority | Gender` roles and never for columns flagged PII by the wizard analysis or bound to `Contact`, `Identifier`, or person-name roles. Generated personas list enum vocabularies but never PII columns.
- **List projection excludes `Contact`/`Identifier` roles by default.** A deterministic list or lookup projects business identifiers, names where the question is about a person, and domain columns; phone, email, national-ID and similar columns are selected only when an admin names them explicitly in the question.
- **Provenance payloads are PHI-free.** `provenance` events and persisted `provenance` fields contain rung names, schema-level miss reasons ("bind error: no EventTime on Bill"), backend, scope table names, source ID, and timings — never row values, patient keys, or free text from the question.
- **Scope is not authorization.** A service line's bound tables and the `RetrievalFilter` are read allow-lists that keep an agent's answers within its department; they do not grant or deny access. Any user who can call `/agents/<kind>` sees the same scope, and per-user/per-clinic record ACLs remain unimplemented as noted for aggregation.
- **Suggestions never propose PII-role columns**, and focus digests (`top_labels`) exclude person-name columns unless the turn was about that person.

## Deployment requirements

Before handling real PHI:

1. Terminate TLS in front of Rocket and configure clients with the HTTPS hostname.
2. Change Android's base network policy to deny cleartext and configure only the approved HTTPS domain; add certificate pinning if operationally supportable.
3. Pin and trust the DocumentDB CA; remove invalid-certificate acceptance.
4. Move JWT persistence to Windows Credential Manager and Android Keystore-backed storage, and remove webview access to that secret store.
5. Set strict CSP and review markdown URL handling and Tauri capabilities.
6. Sanitize all 5xx bodies and escape user-derived regular expressions.
7. Define audit retention, integrity, access, export, and incident-review procedures.
8. Use independent random secrets and least-privileged, read-only source-database accounts.

## Source plans consolidated

- [13 — Profile + Admin](../old/13-profile-admin-implementation.md): current user/admin surface; its original “tokens remain valid” decision is superseded by implemented token-version revocation.
- [14 — Audit Log](../old/14-audit-log-implementation.md): audit read/write design; its “no logout route” statement is superseded by the current logout routes.
- [15 — Polish and Android](../old/15-stage-11-polish-android.md): Android cleartext transport decision and deferred device validation.
- [16 — Security hardening](../old/16-security-hardening.md): threat model and backlog; several items formerly marked proposed are now implemented as identified above.
- [25 — Concurrent chat orchestration](../old/25-concurrent-chat-orchestration.md): stream cancellation and logout-aborts-active-runs behavior.
- [Older security architecture](../old/docs/security-and-auth-architecture.md): consolidated here; stale open-item claims are corrected from current code.
