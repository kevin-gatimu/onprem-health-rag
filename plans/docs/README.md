# Documentation Index

> Reference docs for the on-premises health-records RAG system. This folder contains the curated, engineer-facing guides synthesized from the `plans/` design notes (e.g., `plans/00-master-plan.md`, `plans/01-retrieval-design.md`).

## Quick Links

### Core System
- **[Architecture Overview](architecture-overview.md)** — Components, traffic flow, stack, data model, REST API, module structure. Start here.

### Security & Auth
- **[Security & Auth Architecture](security-and-auth-architecture.md)** — JWT bearer auth, argon2id, RBAC, token-version session revocation + logout, credential encryption (AES-256-GCM), login throttle, audit log, fail-fast config, and the implemented-vs-planned scorecard.

### Retrieval & Chat
- **[Retrieval Pipeline](retrieval-pipeline.md)** — Per-turn semantic RAG: query rewrite → multi-query → hybrid (vector + full-text) → RRF → reranking → grounded generation.

### Advanced Retrieval & Analytics

- **[Aggregation-Aware Retrieval](aggregation-aware-retrieval.md)** — How the app answers counting / grouping / trend questions from the database instead of hallucinating from passages. Health Query / Trends / Analytics / Outbreak Alerts screens powered by this. (Phases 0–3 shipped.)

### Models & Hardware
- **[LLM Selection & Task-Aware Routing](model-selection-and-routing.md)** — Which model for each job (chat, patient lookup, trends, summarize, multi-hop); how to manage memory on a shared-RAM iGPU via lazy load/unload.
- **[Accelerator & Hardware Detection](accelerator-and-hardware-detection.md)** — Physical hardware discovery (CPU/GPU/NPU), Foundry EP registration, graceful fallback when accelerators aren't yet online.

### Operations & Monitoring
- **[Log Streaming](log-streaming.md)** — Real-time server tracing → app panels. Watch long operations (model downloads, embeddings, ingestion) live without checking the server terminal.

### Technical Verification
- **[Live Verification & Decode Findings](decode-and-verification-notes.md)** — Proven facts about DocumentDB `cosmosSearch` syntax, SQL Server type decoding, and UUID ordering. Everything here was tested against real containers.

### Features & Requirements (Curated)
- **[App Feature Surface & Requirements](app-feature-surface-and-requirements.md)** — Target UI screens (Dashboard, Connections, Ingest, Data Explorer, AI Chat, AI Agents, Models, Analytics, Outbreak Alerts, Audit Log, Settings). Gap table vs. current build.

---

## How to Use

**New to the project?**
1. Read [Architecture Overview](architecture-overview.md) for the system topology
2. Skim [Retrieval Pipeline](retrieval-pipeline.md) to understand the chat path
3. Check [App Feature Surface](app-feature-surface-and-requirements.md) to see what's shipping vs. planned

**Working on a specific workstream?**
- **Auth / login / sessions / security:** [Security & Auth Architecture](security-and-auth-architecture.md)
- **Settings / Hardware:** Read [Accelerator & Hardware Detection](accelerator-and-hardware-detection.md) + [LLM Selection & Task-Aware Routing](model-selection-and-routing.md)
- **Retrieval / Chat:** [Retrieval Pipeline](retrieval-pipeline.md)
- **Ingestion:** [Architecture Overview](architecture-overview.md) (collections/modules) + [Log Streaming](log-streaming.md) (monitoring)
- **Data Explorer / Analytics:** [Aggregation-Aware Retrieval](aggregation-aware-retrieval.md)
- **Debugging:** [Live Verification & Decode Findings](decode-and-verification-notes.md) (what's already proven)

**Troubleshooting?**
- **Why did my session drop / how does logout work?** [Security & Auth Architecture](security-and-auth-architecture.md) → Sessions, logout & revocation
- **OpenVINO not registered:** [Accelerator & Hardware Detection](accelerator-and-hardware-detection.md) → EP Registration Flow
- **MSSQL column gone null:** [Live Verification & Decode Findings](decode-and-verification-notes.md) → SQL Server Decode
- **What's the `/chat` SSE payload?** [Retrieval Pipeline](retrieval-pipeline.md) → Grounded Generation
- **Where do I add a new UI page?** [App Feature Surface](app-feature-surface-and-requirements.md)

---

## Doc Relationships

```
Architecture Overview
  ├─▶ Security & Auth Architecture (how login / the JWT guard / logout work)
  ├─▶ Retrieval Pipeline (how /chat works)
  ├─▶ Aggregation-Aware Retrieval (how /agents work; Phases 0–3 shipped)
  ├─▶ LLM Selection & Routing (which model for each task; Phases 0–3 shipped)
  ├─▶ Accelerator & Hardware Detection (how /hardware works)
  └─▶ Log Streaming (monitoring & debugging)

Security & Auth Architecture
  └─▶ plans/16-security-hardening.md (threat model + full remediation roadmap)

App Feature Surface & Requirements
  ├─▶ Architecture Overview (what we're building)
  ├─▶ LLM Selection & Routing (the AI Agents tabs; Phases 0–3 shipped)
  └─▶ Aggregation-Aware Retrieval (Health Query / Trends / Analytics; Phases 0–3 shipped)

Live Verification & Decode Findings
  ├─▶ Retrieval Pipeline (DocumentDB cosmosSearch is verified)
  └─▶ Architecture Overview (connector decode is verified)

Mind Cache (plans/ folder)
  └─▶ 06-agents-routing-and-aggregation.md (Phases 0–3 shipped; what's open in Phase 4)
```

---

## Conventions

- **Cross-links:** `[Link Text](file.md)` for docs in this folder; `[[memory-key]]` for persistent memory entries
- **Code references:** Full module paths (e.g., `onprem-rag-server/src/retrieval/`) and config keys (e.g., `ONPREM_CHAT_MODEL`)
- **Model names:** Full variants where specific (e.g., `qwen2.5-7b-instruct-openvino-gpu:2`); bare aliases when swappable (e.g., `qwen2.5-7b`)
- **Provenance:** Each synthesized doc ends with `Source: synthesized from plans/NN-*.md`

---

## Related Folders

- **`plans/`** — Raw numbered design notes (00-master-plan, 01-retrieval-design, 02-verification, 03-ws6, 04-logs, 05-hardware, 06-agents-routing-and-aggregation). The "mind cache" — updated as decisions are made. Docs here (in `plans/docs/`) synthesize & organize them for reference.
- **`.env.example`** — Configuration template; copy to `.env` and adjust
- **`docker-compose.yml`** — Local DocumentDB + dev-sources containers
- **`onprem-rag-server/`** — Rust Rocket server (modules mirror the doc structure)
- **`onprem-rag-app/`** — Tauri v2 desktop app (React frontend + Rust bridge)

---

Last updated: 2026-08-26 (added Security & Auth Architecture: token-version logout + centralized forced-logout shipped)
