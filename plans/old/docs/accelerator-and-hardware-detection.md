# Accelerator & Hardware Detection

> **Superseded.** This file is retained for link/history stability. The authoritative replacement is [Models, Accelerators, and Lifecycle](models-accelerators-and-lifecycle.md). Do not treat the statuses, defaults, or diagrams below as current.

> How the system discovers and reports physical hardware (CPU, GPU, NPU) and manages Foundry execution providers (EPs) for optimal model selection. Companion docs: [`architecture-overview.md`](architecture-overview.md), [`model-selection-and-routing.md`](model-selection-and-routing.md).

## Problem Solved

Foundry Local's `discover_eps()` reports only **registered EPs** — which ones Foundry has explicitly downloaded and initialized. This conflates two separate facts:

1. **What accelerators are physically on the host?** (e.g., Intel Arc GPU, AI Boost NPU, CPU)
2. **What EPs does Foundry have loaded?** (registered/not registered)

On a Meteor Lake laptop with an Arc GPU + NPU, `discover_eps()` used to show:
```
OpenVINOExecutionProvider  registered: false
WebGpuExecutionProvider    registered: false
```

Confusing, because the hardware is clearly present. Additionally, the pinned default model `qwen2.5-7b-instruct-openvino-gpu:2` would fail with "unknown variant" until OpenVINO was registered — a chicken-and-egg problem.

## Solution

### Physical Hardware Detection (OS-level, Best-Effort)

Independent of Foundry, probe the OS for actual hardware presence. Windows-only (PowerShell CIM queries); returns `[]` elsewhere.

**API:** `GET /hardware` returns:
```json
{
  "execution_providers": [...],
  "detected_hardware": [
    {"kind": "CPU", "name": "Intel Core Ultra 7 155H", "vendor": "Intel"},
    {"kind": "GPU", "name": "Intel Arc GPU", "vendor": "Intel"},
    {"kind": "NPU", "name": "AI Boost NPU", "vendor": "Intel"}
  ]
}
```

**UI message:** *"AI Boost NPU detected but OpenVINO not yet registered"* — clarifies the distinction.

### EP Classification & Labeling

Classify each EP into `{device_kind, label}` so the UI groups by accelerator type instead of showing raw `*ExecutionProvider` strings.

| Foundry EP | device_kind | Label |
|------------|-------------|-------|
| `CPUExecutionProvider` | CPU | CPU |
| `WebGpuExecutionProvider` | GPU | WebGPU / DirectML — GPU |
| `CUDAExecutionProvider` | GPU | CUDA — NVIDIA GPU |
| `*TensorRt*ExecutionProvider` | GPU | TensorRT — NVIDIA GPU |
| `OpenVINOExecutionProvider` | GPU | OpenVINO — Intel GPU/NPU |
| `QNNExecutionProvider` | NPU | QNN — Qualcomm NPU |
| `*Vitis*ExecutionProvider` | NPU | Vitis AI — AMD/Xilinx NPU |
| (anything else) | Other | (raw name) |

### Model Resolution & Graceful Fallback

- **Default chat model:** `qwen2.5-7b` ← **bare alias, not a pinned variant**
- The Foundry SDK resolves the best variant for the registered EPs
- Stays valid as EPs come online post-boot (no "unknown variant" error)
- `resolve(alias)` fails gracefully: on a miss, list available aliases in the error instead of an opaque SDK message

## EP Registration Flow

### Problem: In-Process Core Registration

The Foundry SDK runs the native core **in-process** (via `CoreInterop`). Running `foundry model list` from the CLI registers EPs for the **CLI's service instance** — a *different* core from the server's in-process one. So our server sees only the always-on `CPUExecutionProvider` and not GPU/NPU variants.

### Solution: Programmatic Registration

The SDK exposes `FoundryLocalManager::download_and_register_eps(names: Option<&[&str]>)`, which registers all available EPs into the **server's own core**.

**Workflow:**
1. On server startup, call `spawn_startup_registration()` (fire-and-forget via `tokio::spawn`)
2. EPs download + register silently in the background (may download plugin binaries; non-fatal on timeout)
3. Logs appear in the live log stream (`onprem_server::foundry: Registered OpenVINO ExecutionProvider`)
4. Next `/hardware` call sees `registered: true` for previously unavailable EPs

**Retry:** Admin can trigger explicit re-registration via `POST /hardware/register-eps` (e.g., after a Foundry upgrade that added new plugins).

## Server Implementation

### `foundry/hardware.rs` (New)

Probes OS-level hardware via PowerShell (Windows) / shell (other):

```rust
pub struct DetectedDevice {
    pub kind: String,  // "CPU" | "GPU" | "NPU"
    pub name: String,
    pub vendor: String,
}

pub fn detect() -> &'static [DetectedDevice]  // cached in OnceLock
```

**Windows queries:**
- `Win32_Processor` → CPU name
- `Win32_VideoController` → GPU names
- `Win32_PnPEntity` (name-matched on `AI Boost|Neural Processor|NPU|Hexagon|VPU`) → NPU names

**Vendor inference:** Parse the name for Intel, NVIDIA, AMD, Qualcomm, etc.

**Non-Windows / failures:** Returns `[]` (logged `warn`); never blocks the EP list.

### `foundry/mod.rs` (Extended)

```rust
pub struct ExecutionProvider {
    pub name: String,
    pub registered: bool,
    pub device_kind: String,    // new
    pub label: String,          // new
}

impl ExecutionProvider {
    pub fn classify() -> Self { /* map name to (device_kind, label) */ }
}

pub struct EpRegistration {
    pub success: bool,
    pub status: String,
    pub registered: Vec<String>,
    pub failed: Vec<String>,
}

pub async fn register_eps(&self) -> AppResult<EpRegistration>
    // Calls download_and_register_eps(None), logs results

pub async fn spawn_startup_registration(&self)
    // Fire-and-forget tokio::spawn, non-fatal
```

### `foundry/routes.rs` (Extended)

- **`GET /hardware`** — calls `hardware()`, which builds classified EPs + attaches `detected_hardware`
- **`POST /hardware/register-eps`** (admin) — `register_eps()` for explicit retry

### Config

**`.env` / `.env.example`:**
```
ONPREM_CHAT_MODEL=qwen2.5-7b
```

(Previously pinned to `qwen2.5-7b-instruct-openvino-gpu:2`; now bare alias.)

## Bridge & Frontend

### Bridge Types (`src-tauri/src/lib.rs` & `bridge.ts`)

```typescript
interface ExecutionProvider {
  name: string;
  registered: boolean;
  device_kind: string;    // new
  label: string;          // new
}

interface DetectedDevice {
  kind: "CPU" | "GPU" | "NPU";
  name: string;
  vendor: string;
}

interface HardwareInfo {
  execution_providers: ExecutionProvider[];
  detected_hardware: DetectedDevice[];  // new
}

interface EpRegistration {
  success: boolean;
  status: string;
  registered: string[];
  failed: string[];
}
```

### Bridge Commands

```typescript
async function registerEps(): Promise<EpRegistration>
    // POST /hardware/register-eps, returns status
```

### Frontend (`Settings.tsx`)

**Accelerators section** (replaces "Execution providers"):
- **Group EPs by `device_kind`** (CPU / GPU / NPU / Other)
- Render each as a **chip** using `label` (e.g., "OpenVINO — Intel GPU/NPU")
- **Registered state:** checkmark (registered) vs. dimmed/"available" (not yet registered)
- Tooltip shows raw `name`
- **Detected hardware block** (shown only when non-empty):
  - List of physical devices (kind · vendor · name)
  - Caption: *"Detected hardware — devices available on this machine. Open 'Register providers' to activate in Foundry."*
- **Register providers button** (admin-only, next to Accelerators heading):
  - On click: calls `registerEps()`, shows a small status line
  - Progress visible in "Model & hardware activity" log console
  - Completion refreshes the hardware display

## Hardware Constraints & Model Selection

The physical hardware limits which models can load and run concurrently:

- **7–8B quantized:** runs comfortably on Intel Arc iGPU
- **12B:** slower
- **14B+ / 20B:** painful
- **NPU variants (OpenVINO-npu):** context-capped at 4224 tokens — **avoid for RAG** (which stuffs multiple passages + history)

**Implication:** Always prefer `generic-gpu` variants for chat/agents; park small extractors/verifiers on the NPU if available (see `model-selection-and-routing.md` for roles).

## Workflow & Boot-Time Behavior

1. **Server starts**
   - Initialize Foundry (in-process)
   - Discover registered EPs (may be only CPU at first)
   - Fire-and-forget `spawn_startup_registration()` to download/register all available EPs
   - Start listening on `:8000`

2. **User hits Settings**
   - `GET /hardware` returns classified EPs + detected hardware
   - If EPs registration is still running, UI shows dimmed/"registering" state
   - After ~30–60s, admin can manually refresh or check logs

3. **Chat model resolution**
   - On first `/chat` request, resolve `qwen2.5-7b` (bare alias)
   - Foundry SDK picks the best variant for registered EPs
   - Load the model (cached if already loaded)
   - Generate

4. **Admin retry**
   - Click "Register providers" button
   - `POST /hardware/register-eps` → `register_eps()`
   - Logs visible in "Model & hardware activity" console
   - Refresh hardware display on completion

## Verification (During Development)

1. **Compile:** `cargo check` in both Rust crates (no new warnings beyond pre-existing benign ones)
2. **Types:** `npx tsc --noEmit` in the frontend
3. **Manual:**
   - After server restart (post-Foundry upgrade), `GET /hardware` shows classified EPs + detected hardware
   - OpenVINO shows `registered: true` (on Meteor Lake w/ Intel Arc)
   - Settings screen displays grouped accelerators + detected hardware block

---

Source: synthesized from `plans/05-accelerator-detection.md`
