# 24 — Versioning & updates

Status: **design note** — versioning cleanup is DONE; the updater plugin itself is
NOT yet implemented. This doc captures the agreed design so implementation is a
mechanical follow-up.

## What's already done (this pass)

- `src-tauri/Cargo.toml` + `tauri.conf.json`: real description/authors/productName
  ("OnPrem RAG") replacing the "A Tauri App" / "you" scaffold values.
- Settings page shows an About footer: app version (`getVersion()` from
  `@tauri-apps/api/app`) + server version (`server_specs.server_version`, which is
  the server crate's `CARGO_PKG_VERSION`).
- Single source of truth for the app version is `tauri.conf.json` `version` —
  bump it there per release. The server version is `onprem-rag-server/Cargo.toml`.

## Why the updater is deferred

`tauri-plugin-updater` needs two operational pieces that don't exist yet and
shouldn't be invented mid-feature:

1. **Signing keypair.** `pnpm tauri signer generate` produces a keypair; the
   public key goes in `tauri.conf.json`, the private key + password live ONLY on
   the machine that builds releases. Losing it means shipped apps can never
   update again (key is pinned); leaking it means anyone can feed clients
   malicious updates. Decide storage (offline / password manager) first.
2. **A place to serve updates.** This is an on-prem, no-internet deployment —
   public GitHub release URLs are out. Serve from the Rocket server instead.

## Agreed design (when implemented)

- **Desktop only.** Android sideloads APKs / uses MDM; the updater plugin goes
  under `[target.'cfg(not(target_os = "android"))'.dependencies]` and is
  registered inside the existing `#[cfg(desktop)]` block in `lib.rs`.
- **Server-hosted manifest.** New route `GET /updates/latest.json` on the Rocket
  server, serving files from `ONPREM_UPDATES_DIR` (env-configured, absent = 404
  and the client treats it as "no updates channel"). Admin drops
  `latest.json` + signed bundles into that dir per release.
- **Artifacts.** `"createUpdaterArtifacts": true` under `bundle` in
  `tauri.conf.json`; CI/build machine signs with the private key.
- **Client UX.** Check on launch (and via a "Check for updates" button in the
  Settings About area); notify + prompt, never auto-install — this is a clinical
  workstation, an update mustn't interrupt an active session.
- **Endpoint config.** The updater endpoint derives from the same server base URL
  the bridge already holds — no separate update-server setting.

## Implementation checklist (future pass)

- [ ] Generate + safely store signing keypair; pubkey into `tauri.conf.json`.
- [ ] `bundle.createUpdaterArtifacts: true`.
- [ ] Add `tauri-plugin-updater` (desktop-only dep) + `updater:default` capability.
- [ ] Rocket: `GET /updates/latest.json` + bundle files from `ONPREM_UPDATES_DIR`
      (unauthenticated — manifest/bundles contain no PHI, and the updater runs
      pre-login; integrity comes from the signature, not transport auth).
- [ ] Bridge command `check_for_update` + Settings UI hook.
- [ ] Release runbook: bump version → build/sign → copy to updates dir.
