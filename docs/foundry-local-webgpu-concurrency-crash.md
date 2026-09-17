# A ~20 KB prompt silently kills the process hosting a Foundry Local model

**Status:** root cause identified, mitigated in this repo, not fixed upstream
**Impact:** the entire process hosting the model dies — no panic, no log line, no error to the client
**Component:** `onnxruntime_providers_webgpu.dll` 1.26.20260522.3.d2ede0a, reached through
`onnxruntime_genai` → `Microsoft.AI.Foundry.Local.Core.dll` 1.2.0 / `foundry-local-sdk` 1.2.3

> **Correction.** This was first filed as a *concurrency* bug, because it reproduced reliably
> with two overlapping chat requests and never with one. That was a correlation, not the cause:
> the second request in our pipeline is the one that carries a large prompt. A single request
> with a 26.5 KB prompt and no concurrency whatsoever kills the host just as reliably. The
> concurrency findings are kept below under "What was ruled out" because the eliminations are
> still useful, but **prompt size is the trigger**.

---

## Summary

Above roughly 20 KB of prompt, ONNX Runtime's WebGPU execution provider corrupts its own device
state and the hosting process dies by `__fastfail`. The context window is nowhere near full
(`qwen3-8b` has 40,960 tokens; the failing prompt is ~5,000). There is no error return, no
exception the caller can catch, and nothing written to the host's log.

The device errors are visible in Foundry's own core log just before death:

```text
WebGPU device error(2): [Invalid CommandBuffer] is invalid due to a previous error.
 - While calling [Queue].Submit([[Invalid CommandBuffer]])
   (webgpu_context.cc:99 onnxruntime::webgpu::WebGpuContext::Initialize)
WebGPU device error(2): [CommandEncoder (unlabeled)] is already finished.
 - While encoding [CommandEncoder].CopyBufferToBuffer([Buffer], 0, [Buffer], 0, 50331648)
```

A 50,331,648-byte (48 MiB) buffer copy, and a command encoder being reused after it was
finished. Once the WebGPU context reaches this state, everything after it is invalid.

## Measured threshold

Same host, same model (`qwen3-8b-generic-gpu:2`), one request at a time, no concurrency.
Prompt is filler text plus "Summarise in one sentence.", `max_tokens: 40`.

| Prompt bytes | Result |
| --- | --- |
| 8,196 | answers normally |
| 16,336 | answers normally |
| 19,396 | answers normally |
| 22,456 | **host dies** |
| 26,536 | **host dies** |

So the cliff is between ~19.4 KB and ~22.5 KB — about 5,000 tokens, against a 40,960-token
context window.

## Environment

| Item | Value |
| --- | --- |
| OS | Windows 11 Home 26200 (build 26100) |
| CPU | Intel Core Ultra 7 155H |
| GPU | Intel Arc Graphics (integrated), driver 32.0.101.8132 |
| RAM | 32 GB (integrated GPU shares system RAM — no dedicated VRAM) |
| `foundry` CLI / daemon | 0.10.3 (`foundrylocald.exe`) |
| `foundry-local-sdk` | 1.2.3, `winml` feature |
| Native core | `Microsoft.AI.Foundry.Local.Core.dll` 1.2.0 |
| WebGPU EP | `onnxruntime_providers_webgpu.dll` 1.26.20260522.3.d2ede0a |
| ORT / ORT-GenAI | 1.26.0 / 0.0.0 (per `~/.foundry/daemon.json`) |
| Models seen failing | `qwen3-8b-generic-gpu:2`, `qwen3-14b-generic-gpu:2` |

## Reproduction

Minimal, against the daemon alone — no application code involved.

```bash
foundry server start
foundry model load qwen3-8b

# Build a ~26 KB prompt and send one request.
python - > big.json <<'PY'
import json
line = "Patient record: id=%d, name=Test Patient, ward=General, medication=Amoxicillin 500mg.\n"
body = "".join(line % i for i in range(260))
print(json.dumps({"model": "qwen3-8b",
                  "messages": [{"role": "user", "content": body + "\nSummarise. /no_think"}],
                  "max_tokens": 40}))
PY

URL=$(python -c "import json;print(json.load(open(r'C:\Users\kevin\.foundry\daemon.json'))['web_urls'][0])")
curl -sS -m 400 -X POST "$URL/v1/chat/completions" \
  -H 'content-type: application/json' --data-binary @big.json

foundry server status     # State: Not running
```

**Expected:** an answer, or an error explaining the limit.
**Actual:** `curl: (56) Recv failure: Connection was reset`, and the daemon is gone. Its log ends
mid-generation with the WebGPU device errors above and no shutdown record.

Replace `range(260)` with `range(190)` (~19 KB) and the same request answers normally.

## In-process, the same fault kills the calling application

`foundry-local-sdk` runs the model inside the calling process (see "The SDK has no service mode"
below). There, the identical fault is far worse: the C++ exception is raised on one of the native
core's **own thread-pool threads**, where no caller frame exists to catch it, so the MSVC unwinder
finds no handler and calls `terminate()` → `abort()`. Windows fail-fasts the application with
`STATUS_STACK_BUFFER_OVERRUN` (`0xC0000409`, subcode `FAST_FAIL_FATAL_APP_EXIT`).

Nothing is printed. A Rust host sees only:

```text
error: process didn't exit successfully: `target\debug\onprem-server.exe`
       (exit code: 0xc0000409, STATUS_STACK_BUFFER_OVERRUN)
```

Stack captured with `cdb -p <pid> -g -c "g"` then `kf 80`, reading bottom-up:

```text
ntdll!RtlUserThreadStart
kernel32!BaseThreadInitThunk
ntdll!TpCallbackMayRunLong                                  <-- native core's own thread pool
Microsoft_AI_Foundry_Local_Core!execute_command_with_binary+0x35a2c7
  ... (23 frames inside Microsoft.AI.Foundry.Local.Core) ...
onnxruntime_genai!OgaGenerator_AppendTokenSequences+0x15f   <-- generation in progress
onnxruntime!OrtSessionOptionsAppendExecutionProvider_CPU+0x48a465
onnxruntime_providers_webgpu!ReleaseEpFactory+0x103172      <-- WebGPU EP
  ... (7 frames inside onnxruntime_providers_webgpu) ...
VCRUNTIME140!CxxThrowException+0x99                         <-- throws here
ntdll!KiUserExceptionDispatcher+0x2e
VCRUNTIME140_1!_CxxFrameHandler4+0xa5
VCRUNTIME140_1!FindHandler<__FrameHandler4>+0x47b           <-- no handler found
ucrtbase!terminate+0x1e
ucrtbase!abort+0x4e                                         <-- 0xC0000409
```

The two ends are the point: the thread was **started by the native core**, and the exception
**found no handler**. The host application is not on this stack and cannot intervene.

## What was ruled out

Each was tested against the reproduction and did **not** prevent the crash.

| Hypothesis | Test | Result |
| --- | --- | --- |
| Concurrent generations | Process-wide semaphore(1) around every native entry point (chat, fastembed embed, fastembed rerank), with acquire/release logging | Still crashes. The gate timeline showed one call active; a full-thread dump confirmed exactly one generating thread |
| Memory / GPU-memory exhaustion | `qwen3-14b` → `qwen3-8b`, dropping RSS 17.7 GB → 8.2 GB on a 32 GB host | Still crashes, identically |
| Teardown race between generations | Forced settling windows of 1.5 s and 15 s between generations | Still crashes at both |
| Context-window overflow | Model reports 40,960 tokens; failing prompt ≈ 5,000 | Not the limit |
| Host-side bug (Rust panic, corruption) | `RUST_BACKTRACE=full`, no panic hook, `panic = abort` not set | No Rust panic occurs; the exception is C++ (`e06d7363`) from native code |

Concurrency reproduced it reliably only because, in this pipeline, the second request is the
unscoped one whose planner prompt carries all 63 schema tables (~25 KB) while the first is
department-scoped (~9 KB). Serialising requests removes the crash exactly when it also removes
the large prompt — which is what made concurrency look causal for so long.

## The SDK has no service mode

Verified against `foundry-local-sdk` 1.2.3 source, because two things look like they would move
inference out of process and neither does:

- **`winml` is not a mode switch.** It is the crate's only feature flag and its entire effect is
  pre-loading one extra DLL (`Microsoft.Windows.AI.MachineLearning.dll`) in
  `detail/core_interop.rs`. With or without it, the core loads `onnxruntime.dll` and
  `onnxruntime-genai.dll` into the calling process.
- **`FoundryLocalManager::start_web_service()` is not isolation.** It calls
  `core.execute_command_async("start_service")` — a web service *hosted by the in-process core*.
  An HTTP endpoint, and none of the fault containment.

`ChatClient` holds an `Arc<CoreInterop>` and calls `core.execute_command_async("chat_completions")`.
There is no HTTP path for generation in the crate. What the SDK *does* offer against an external
daemon is `FoundryLocalConfig::service_endpoint(url)` (serialised as `WebServiceExternalUrl`),
read only by `ModelLoadManager`, which then routes `GET {base}/models/load/{id}`,
`GET {base}/models/unload/{id}` and `GET {base}/models/loaded` over HTTP.

## Fault tolerance: daemon vs. in-process

The daemon does not prevent the crash — the threshold table above was measured against the daemon.
What it changes is who dies and whether anyone finds out.

| | In-process (SDK `ChatClient`) | `foundry` daemon (HTTP) |
| --- | --- | --- |
| Oversized prompt | **kills the host application**, silently | kills the daemon; caller gets a connection reset |
| Recovery | restart the whole server | restart the daemon; the API stays up |
| Ordinary ONNX errors | cannot be reported at all | returned as a JSON `500` |
| Visibility | nothing logged anywhere | WebGPU device errors in `~/.foundry/logs/foundry.core*.log` |

An example of the third row, from a genuinely bad model/shape combination:

```json
{"error":{"message":"Failed to handle OpenAI completion: Non-zero status code returned while
 running GroupQueryAttention node ... Input 'query' and 'key' shall have same dim 1
 (sequence length)","type":"server_error","code":null}}
```

That class of failure is simply unreportable in-process.

## Mitigations applied in this repository

1. **Bound the prompt.** `ONPREM_PROMPT_MAX_BYTES` (default 16,000) clamps system + user at the
   single point every prompt passes through, trimming the schema catalog rather than the question
   and warning with the caller's name. This is a safety net, not a fix: a truncated catalog costs
   answer quality, and the real remedy is to send less.
2. **Run generation in the daemon.** `ONPREM_FOUNDRY_BACKEND=service` (default) — see
   `plans/docs/foundry-service-migration.md`. A fault then kills a restartable child, and the API
   returns `503` instead of vanishing.
3. **One generation at a time.** `ONPREM_MAX_ACTIVE_GENERATIONS=1`, with a long queue wait
   (`ONPREM_GENERATION_QUEUE_TIMEOUT_MS`, default 300 s) so a second asker waits rather than being
   rejected. Retained because a model host serving one request is easier to reason about, and
   because eviction churn (`unload` then `load`) has also been observed to kill the daemon.

## Open questions for upstream

- Why does ~20 KB of prompt produce a 48 MiB `CopyBufferToBuffer` and an already-finished command
  encoder, with 40,960 tokens of context available?
- Is the threshold a WebGPU buffer/binding limit on this adapter, and can the EP fall back or
  return an error rather than invalidating its device?
- Can the native core catch on its own thread-pool threads, so an in-process host gets an error
  instead of `abort()`? Today any EP fault is unconditionally fatal to the embedding application.
