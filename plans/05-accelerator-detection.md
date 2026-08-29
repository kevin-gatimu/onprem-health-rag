# 05 — Accelerator detection & model-selection robustness

> Design note for making the Settings/Hardware screen honestly report the machine's
> accelerators, and for keeping chat model selection working as Foundry execution
> providers (EPs) come online. Started 2026-08-23.

## Problem

`GET /hardware` delegated straight to Foundry Local's `discover_eps()`, which reports only
which **EPs are registered**. On a Meteor Lake laptop with an Intel Arc GPU + NPU, that showed:

```
OpenVINOExecutionProvider  registered: false
WebGpuExecutionProvider    registered: false
```

— confusing, because the hardware is clearly present. Two orthogonal facts were conflated:
1. **What accelerators does Foundry target?** (its EP list: CPU / WebGPU / CUDA / TensorRT /
   OpenVINO / QNN / Vitis AI)
2. **What physical hardware is actually on the host?** (GPU/NPU/CPU devices)

Separately, the configured default `ONPREM_CHAT_MODEL=qwen2.5-7b-instruct-openvino-gpu:2` was a
**pinned variant id** that isn't in the catalog until the OpenVINO EP registers — so the first
`/generate` failed with an opaque "unknown variant".

Update this session: user upgraded Foundry Local 0.8.119 → **0.10.3**; `foundry model list` then
**downloaded + registered OpenVINOExecutionProvider**. So after a **server restart**,
`discover_eps()` should report OpenVINO registered. The UI work below still stands — it explains
the gap whenever an EP is present-but-unregistered.

## Target accelerator taxonomy (user-specified)

The UI must be able to name each of these Foundry EPs clearly:

| EP name (SDK)                     | device_kind | label                         |
| --------------------------------- | ----------- | ----------------------------- |
| `CPUExecutionProvider`            | CPU         | CPU                           |
| `WebGpuExecutionProvider` (/ DML) | GPU         | WebGPU / DirectML — GPU       |
| `CUDAExecutionProvider`           | GPU         | CUDA — NVIDIA GPU             |
| `*TensorRt*ExecutionProvider`     | GPU         | TensorRT — NVIDIA GPU         |
| `OpenVINOExecutionProvider`       | GPU         | OpenVINO — Intel GPU/NPU      |
| `QNNExecutionProvider`            | NPU         | QNN — Qualcomm NPU            |
| `*Vitis*ExecutionProvider`        | NPU         | Vitis AI — AMD/Xilinx NPU     |
| anything else                     | Other       | (raw name)                    |

## Decisions

- **Keep Foundry's EP list as the source of truth for what's *usable*** (registered), but
  classify each EP into `{device_kind, label}` so the UI groups by accelerator instead of
  showing raw `*ExecutionProvider` strings. Classification is a pure string map — no new deps.
- **Add independent OS-level physical detection** (`detected_hardware`) so the UI can say
  "device present, EP not registered yet". Best-effort, cached for process life, Windows-only
  (PowerShell CIM); returns `[]` elsewhere and on any failure. Never blocks the EP list.
- **Default chat model → bare alias `qwen2.5-7b`** (not a pinned variant). The SDK resolves the
  best variant for the registered EPs, so it stays valid as EPs come online. Pinning is opt-in.
- **`resolve()` fails gracefully**: on a miss, list available aliases in the error instead of an
  opaque SDK message.

## Implementation

### Server (`onprem-rag-server/src/foundry/`)

- **`hardware.rs` (new)** — OS-level detection.
  - `DetectedDevice { kind: "CPU"|"GPU"|"NPU", name, vendor }` (`Serialize`).
  - `detect() -> &'static [DetectedDevice]` cached in a `OnceLock`.
  - `#[cfg(windows)]`: one `powershell -NoProfile -NonInteractive -Command` round-trip running
    CIM queries — `Win32_Processor` (CPU), `Win32_VideoController` (GPUs), `Win32_PnPEntity`
    name-matched on `AI Boost|Neural Processor|\bNPU\b|Hexagon|VPU` (NPUs) — emitting compact
    JSON, parsed with serde. `vendor_of()` infers Intel/NVIDIA/AMD/Qualcomm/… from the name.
    Non-Windows / any failure → `[]` (logged `warn`).
- **`mod.rs`** —
  - `pub mod hardware;`
  - `ExecutionProvider` gains `device_kind` + `label`; `ExecutionProvider::classify()` implements
    the table above; `::new(name, registered)` fills them in.
  - `HardwareInfo` gains `detected_hardware: Vec<DetectedDevice>`.
  - `hardware()` builds classified EPs and attaches `hardware::detect().to_vec()`.
  - `resolve()` on miss returns `AppError::Unavailable` listing sorted, deduped aliases.
- **`config.rs`** + **`.env`** / **`.env.example`** — default `ONPREM_CHAT_MODEL=qwen2.5-7b`.

### Frontend (`onprem-rag-app/src/`)

- **`lib/bridge.ts`** — `ExecutionProvider` gains `device_kind` + `label`; new `DetectedDevice`;
  `HardwareInfo` gains `detected_hardware`. **(done)**
- **`settings/Settings.tsx`** — the "Execution providers" section becomes an **Accelerators**
  view (remaining work — hand to Sonnet):
  - Group EPs by `device_kind` (CPU / GPU / NPU / Other); render each as a chip using `label`,
    with a ✓/registered vs dimmed/"available" state (as today) and the raw `name` in a tooltip.
  - Add a **Detected hardware** line/list from `detected_hardware` (kind · vendor · name), shown
    only when non-empty, with a caption clarifying it's physical presence vs Foundry EP support.
  - Keep "Current chat model" as-is.

## Workflow note

Per Kevin's standing preference, planning is done here; the remaining **Settings.tsx** UI code +
build verification are delegated to a **Sonnet** subagent.

## Verification

- `cargo check` in `onprem-rag-server` (classification + hardware module + resolve fallback).
- `npx tsc --noEmit` in `onprem-rag-app`; `cargo check` in `src-tauri` (no bridge change, but the
  types flow through).
- Manual: restart server, `GET /hardware` via `server.http` — expect classified EPs (OpenVINO now
  `registered: true` post-upgrade) and a populated `detected_hardware` (Intel CPU, Arc GPU, AI
  Boost NPU). Settings screen shows grouped accelerators + detected hardware.

## Follow-up — EP registration into the server's own core (2026-08-23)

**Discovery:** After the upgrade + server restart, `GET /hardware` still showed only
`WebGpu` + `OpenVINO`, both `registered: false`, and no CPU. Root cause: the SDK runs Foundry's
native core **in-process** (`CoreInterop`, no external service URL). Running `foundry model list`
registered OpenVINO for the **CLI's** service instance — a *different* core from the server's.
`discover_eps` reads our in-process core, which never had its plugin EPs registered. (CPU is the
always-on built-in and simply isn't listed by `discover_eps`, but is always usable.)

**Fix:** the SDK exposes `FoundryLocalManager::download_and_register_eps(names: Option<&[&str]>)`
(→ `EpDownloadResult { success, status, registered_eps, failed_eps }`). Passing `None` registers
all EPs available for the hardware — the programmatic equivalent of the CLI's first-run download,
but against *our* core. This makes OpenVINO/WebGPU `registered: true` and usable for chat.

Design:
- **`foundry/mod.rs`** — `EpRegistration { success, status, registered, failed }` (Serialize);
  `register_eps(&self) -> AppResult<EpRegistration>` (awaits `download_and_register_eps(None)`,
  logs registered/failed via `tracing`); `spawn_startup_registration(&self)` — fire-and-forget
  `tokio::spawn` (the `&'static` manager is `Copy`/`Send`) so accelerators come online at boot
  without a manual step, logged to the live log stream (may download plugin binaries; non-fatal).
- **`foundry/routes.rs`** — `POST /hardware/register-eps` (admin) → `register_eps()`, for an
  explicit retrigger/retry the user can watch in the log window.
- **`main.rs`** — after `FoundryManager::init`, `if let Some(f) = &foundry { f.spawn_startup_registration(); }`;
  mount the new route.
- **Bridge** — `commands.rs` `register_eps` command (POST, bearer, like `select_model`),
  registered in `lib.rs`; `bridge.ts` `EpRegistration` + `registerEps()`.
- **`settings/Settings.tsx`** — admin-only "Register providers" button by the Accelerators
  section; on click calls `registerEps()` then `refresh()`, with a small status line. Progress is
  visible in the existing "Model & hardware activity" log window.

Note: registration downloads can be large on a fresh machine; here the plugins are already on
disk (from the CLI run), so registering into our core should be quick.

### Status — implemented (2026-08-23)

- `onprem-rag-server/src/foundry/mod.rs` — `EpRegistration` struct, `register_eps()`, `spawn_startup_registration()`.
- `onprem-rag-server/src/foundry/routes.rs` — `POST /hardware/register-eps` route.
- `onprem-rag-server/src/main.rs` — calls `spawn_startup_registration()` after manager init.
- `onprem-rag-app/src-tauri/src/commands.rs` + `src-tauri/src/lib.rs` — `register_eps` bridge command.
- `onprem-rag-app/src/lib/bridge.ts` — `EpRegistration` interface and `registerEps()`.
- `onprem-rag-app/src/settings/Settings.tsx` — admin "Register providers" button + status line.

Verified: `cargo check` clean (both crates, only 3 pre-existing benign dead-code warnings); `npx tsc --noEmit` clean.

## Status — complete (2026-08-23)

- Server (`hardware.rs`, `mod.rs`, `config.rs`) + env files + `bridge.ts` — **implemented**.
- `Settings.tsx` "Accelerators" view (EPs grouped by kind + detected-hardware block) — **implemented (Sonnet).**
- Verified: `cargo check` clean (only the 3 pre-existing benign dead-code warnings); `npx tsc --noEmit` clean.
- Requires a **server restart** to pick up the upgraded Foundry (OpenVINO now registered) and the new default model.
