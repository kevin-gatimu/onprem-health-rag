# 23 — Model download progress persistence (Models tab)

## Problem

Download progress on the Models tab lives in `VariantCard` **local state**, fed by per-call
`listen()` callbacks inside `bridge.ts pullModel`. Navigating to another tab unmounts the card,
so returning shows no progress even though the download is still running in the Rust bridge.
The `model://*` events also carry **no variant id**, so two concurrent downloads would
cross-wire each other's progress bars.

## Decision

Adopt the same architecture every other stream already uses (chat/agent/ingest):
**envelope events with an id → one boot-time listener in `bridgeEvents.ts` → a zustand store**
that survives navigation. Components render from the store; orchestration (invoke, toasts,
query invalidation) lives in the store action so it also survives unmount.

## Changes

1. **`src-tauri/commands.rs` — `pull_model`**: wrap every `model://progress|status|error|done`
   emit in `ModelEvent { variant_id, data }` (mirror of `ChatEvent { run_id, data }`).
2. **`src/stores/models.ts` (new)**: `downloads: Record<variantId, { pct, status }>` plus a
   `startDownload(variantId)` action that seeds the entry, invokes `pullModel`, and on
   settle toasts + invalidates `['model-roles']` / `['setup-status']` via the module-scoped
   `queryClient`, then clears the entry. Terminal state comes from the invoke promise
   (resolves on `done`, rejects with the error payload) — no done/error listeners needed.
3. **`src/lib/bridge.ts`**: `pullModel(variantId, load)` becomes a plain invoke wrapper;
   drop `PullCallbacks` and the per-call listen/unlisten block. Add the `ModelEvent` mirror.
4. **`src/lib/bridgeEvents.ts`**: register `model://progress` + `model://status` once at boot,
   fanning into `useModels` keyed by `variant_id`.
5. **`VariantCard.tsx`**: delete local `downloading/progress/statusText` state; derive all
   three from `useModels((s) => s.downloads[variant.id])`. `handleDownload` just calls the
   store action. Progress bar + disabled states now persist across tab switches, and each
   card only re-renders for its own variant.

## Non-goals / follow-ups

- `selectModel` ("Load") can also download-if-needed but streams no events; its spinner is
  still local and is lost on navigation (completion toast + invalidation already survive via
  global stores). If it grows progress SSE later, reuse this same store.
- No persistence to disk: a bridge-side download dies with the app process, so rehydrating
  progress across restarts would show a stale bar. Session-lifetime (zustand) is correct.

## Gates

`cargo check` in `onprem-rag-app/src-tauri` · `npx tsc --noEmit` in `onprem-rag-app`.
