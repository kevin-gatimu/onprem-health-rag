# Core LLM requirements

One shared local model (the **Core LLM**) powers every generative role in the app: chat,
health query, trends, patient lookup, summarize, intent classify, query rewrite,
extraction, verification, and text-to-SQL. All roles resolve through the single `"chat"`
router override (`foundry/router.rs::override_key`), so exactly one GPU-class model stays
resident — swapping models mid-request forces an LRU eviction and a multi-second cold
start, which is why per-role model choice was removed.

A model must meet **all** of the following to be offered in the Core LLM selector.

## Hard requirements

| # | Requirement | Why | Enforced |
|---|-------------|-----|----------|
| 1 | **Tool calling** (`Tools: Yes` in the Foundry catalog) | The intent router Tier-2 classify is a forced tool call, and health-query/trends/lookup drive the `run_aggregation` tool. Without it, structured analytics silently degrade to semantic retrieval. | Curated allowlist (`SHARED_LLM_ALIASES` in `foundry/routes.rs`), verified against `foundry model info <alias>` before adding. UI warns per-variant via `supports_tool_calling`. |
| 2 | **Context window ≥ 8,192 tokens** | The nl2sql prompt budget alone is ~3,200 tokens + 256 output, plus chat history and retrieved passages. 4k-window variants overflow in normal use (`exceeds the model's maximum context length` errors). | Hard per-variant filter (`SHARED_LLM_MIN_CONTEXT` in `foundry/routes.rs`); variants with unknown context pass. |
| 3 | **Available as a Foundry Local catalog alias** with at least one variant for this hardware (WebGPU / OpenVINO GPU / CPU) | Chat inference is served exclusively by Foundry Local on-prem — no cloud calls, ever. | Variants are resolved from the local catalog at request time; aliases with no matching variant simply don't render. |

## Soft criteria (judgement, not enforced)

- **Weights fit the accelerator** — prefer ≤ ~8 GB quantized for an Intel iGPU-class
  device; 12B is the practical ceiling, 20B-class models are too slow per token.
- **Reliable ONNX GPU variant** — e.g. `gemma-4-e2b-it-generic-gpu` intermittently fails
  with `GroupQueryAttention … same dim 1` on multi-turn prompts (Foundry/ORT bug); a
  model that meets the hard criteria can still be a poor default.
- **Instruction-tuned chat model** — reasoning-trace models (DeepSeek R1 family) waste
  tokens on `<think>` output and lack tool calling anyway.

## Current allowlist

Verified against the local catalog (`foundry model info <alias>`) on 2026-09-04:

| Alias | Tools | Context | Notes |
|-------|-------|---------|-------|
| `qwen3-4b` / `qwen3-8b` / `qwen3-14b` | Yes | 40,960 | `qwen3-8b` is the recommended default |
| `gemma-4-e2b-it` | Yes | 131,072 | works, but see GQA bug above |
| `mistral-nemo-12b-instruct` | Yes | 131,072 | largest reliable option |
| `olmo-3-7b-instruct` | Yes | 65,536 | |
| `phi-4-mini` | Yes | varies | 4k-context variants (e.g. `…-openvino-gpu:2`) are filtered out by the context gate |

Evaluated and **rejected**:

| Alias | Rejected because |
|-------|------------------|
| `phi-4` | `Tools: No` |
| `mistral-7b-v0.2` | `Tools: No`, context 4,224 |
| `gpt-oss-20b` | `Tools: No` |
| `deepseek-r1-*` | no tool calling; reasoning-trace output |

## Adding a new model

1. `foundry model info <alias>` — confirm `Tools: Yes` and context ≥ 8,192.
2. Add the alias to `SHARED_LLM_ALIASES` in `onprem-rag-server/src/foundry/routes.rs`.
3. Add a display identity (family + parameter weight, e.g. "Qwen3" / "8B") to
   `modelIdentity` in `onprem-rag-app/src/features/models/SharedLlmCard.tsx`.
4. Sanity-check a structured question (a count/group-by) end-to-end — catalog metadata
   has been wrong before, and tool-call failures degrade silently to semantic retrieval.
