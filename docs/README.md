<!-- markdownlint-disable MD012 -->

# Documentation index and authority policy

`plans/docs/` is the authoritative documentation for **current system behavior**. Start here when deciding how the application works today.

Historical design and decision records are archived under [`plans/old/`](../old/). They preserve rationale, alternatives, and implementation history, but their models, phase labels, open-item lists, and proposed behavior may be stale. When a historical plan conflicts with an authoritative guide, use the guide, then verify the claim against the current code and tests.

## In-flight implementation plans

[`plans/new/`](../new/00-README.md) holds the **accepted target design** that is not yet in code: the hospital service-line ontology and per-source `SchemaBinding`, router v3, the `QuerySpec` IR, the single `StructuredExecutor` ladder, service-line agents with Ask/Trends/Handover modes, `ConversationFocus`, and the app restructure. Every guide below that describes this design labels it **Planned** and links to the specific plan. Read [`plans/new/00-README.md`](../new/00-README.md) first for the goals, build order, verified baseline, and shared gates. When a plan is implemented and its behavior is verified against code and tests, the affected guide is updated to **Implemented** in the same change and the plan file moves to [`plans/old/`](../old/). A plan in `plans/new/` is a specification, not evidence of completion.

## Authoritative current guides

Read these first. Status claims in these guides distinguish current implementation from incomplete or proposed work.

### Core architecture and AI patterns

| Guide | Scope |
| --- | --- |
| [System Architecture Design](system-architecture-design.md) | Overall system design: product clients, deployment topology, trust boundaries, runtime components, request paths, streaming, concurrency, failure behavior, and delivery status. |
| [Data Architecture](data-architecture.md) | Data ownership, lineage, provenance, source-row-to-chunk identity, ingest and schema-catalog generations, last-known-good publication, consistency, retention, and deletion semantics. |
| [RAG Patterns](rag-patterns.md) | Implemented retrieval-augmented generation patterns: offline ingestion, hybrid vector/lexical retrieval, rewrite and expansion, RRF, deduplication, reranking, grounding, citations, refusal, verification, and structured RAG. |
| [Agentic Patterns](agentic-patterns.md) | Bounded agentic orchestration: deterministic-first routing, task-aware local model roles, typed and validated plans, guarded execution, fixed fallback graphs, run-scoped streaming, and explicit limits on autonomy. |
| [Role of Foundry Local](foundry-local-role.md) | Foundry Local's role as the in-process local generative-model runtime and lifecycle manager for chat, routing, planning, extraction, verification, and memory tasks; excludes cloud Foundry, embedding, and reranking. |
| [DocumentDB Architecture](documentdb-architecture.md) | DocumentDB's role as the Internal DocumentDB Hybrid Store for copied documents, vectors, retrieval text, metadata, generations, conversations, and application state; distinct from external clinical source databases. |

### Detailed subsystem guides

| Guide | Scope |
| --- | --- |
| [Architecture and data model](architecture-and-data-model.md) | Trust boundaries, runtime components, request paths, DocumentDB collections, identifiers, and generation semantics. Best starting point. |
| [Application and feature architecture](application-and-feature-architecture.md) | Desktop App and Mobile App product surface, shared Tauri/web-view implementation, state and event ownership, responsive shell, and permissions. |
| [Ingestion, schema catalog, and Data Explorer](ingestion-schema-catalog-and-explorer.md) | SQL connectors, paged ingestion, checkpoints, last-known-good generations, schema metadata, drift, and record browsing. |
| [Retrieval, chat memory, and concurrency](retrieval-chat-memory-and-concurrency.md) | Semantic RAG, hybrid retrieval, RRF, reranking, grounding, persisted memory, concurrent runs, queues, and cancellation. |
| [Routing, agents, and structured query](routing-agents-and-structured-query.md) | Tiered intent routing, live-source SQL, DocumentDB aggregation, agent behavior, safety controls, and fallback order as implemented today, plus the **Planned** target design: router v3 (focus resolution, Tier 1.5 deterministic parse, backend from binding coverage, Clarify), the single `StructuredExecutor` ladder with a `provenance` event, and service-line agents. |
| [Hospital agents and their data map](hospital-agents-and-data-map.md) | **Planned.** Service-line ontology and per-source schema binding: the 13 service lines plus Ask, the entity concepts each owns, how a `SchemaBinding` maps any hospital's tables and columns onto them at catalog time, the dev-seed binding as one worked example, and the four code hooks that enforce the read allow-list. |
| [Models, accelerators, and lifecycle](models-accelerators-and-lifecycle.md) | Foundry Local and fastembed boundaries, task roles, execution providers, model residency, downloads, and warmup. |
| [Core LLM requirements](core-llm-requirements.md) | Selection criteria for the shared Core LLM (tool calling, context window, local availability), the current allowlist, and how to add a model. |
| [Security, authentication, and audit](security-auth-and-audit.md) | JWT/RBAC, revocation, secrets, credential encryption, admission controls, audit coverage, risks, and deployment requirements. |
| [Operations, performance, and observability](operations-performance-and-observability.md) | Readiness, limits, caching, instrumentation, metrics, live logs, performance constraints, and validation workflow. |
| [Evaluation, production validation, and release](evaluation-production-and-release.md) | Automated and manual evidence, CI scope, production gates, versioning, updater state, and required release artifacts. |
| [Verified platform notes](verified-platform-notes.md) | Narrow, dated runtime facts proven against explicit environments, plus their invalidation conditions. |
| [Deterministic query handling reference](deterministic-sql-matcher.md) | Detailed reference used by the routing guide: the **Planned** `QuerySpec` IR (grammar, domain predicates, binder, per-dialect compiler, model fallback in the same IR, golden suite) and the current hardcoded template matcher inventory, attempt order, schema requirements, refusal rules, validation, and fixtures. |

The current file, [Documentation index and authority policy](README.md), defines how these documents relate and how they must be maintained.

## Reading paths by role

| Role or workstream | Recommended path |
| --- | --- |
| New contributor | [System Architecture Design](system-architecture-design.md) → [Data Architecture](data-architecture.md) → [Application and feature architecture](application-and-feature-architecture.md) → the relevant subsystem guide → [Evaluation, production validation, and release](evaluation-production-and-release.md). |
| AI/RAG engineer | [RAG Patterns](rag-patterns.md) → [Agentic Patterns](agentic-patterns.md) → [Role of Foundry Local](foundry-local-role.md) → [DocumentDB Architecture](documentdb-architecture.md) → [Evaluation, production validation, and release](evaluation-production-and-release.md). |
| Database/data engineer | [Data Architecture](data-architecture.md) → [DocumentDB Architecture](documentdb-architecture.md) → [Ingestion, schema catalog, and Data Explorer](ingestion-schema-catalog-and-explorer.md) → [Routing, agents, and structured query](routing-agents-and-structured-query.md). |
| Retrieval and chat | [Retrieval, chat memory, and concurrency](retrieval-chat-memory-and-concurrency.md) → [Routing, agents, and structured query](routing-agents-and-structured-query.md) → [Models, accelerators, and lifecycle](models-accelerators-and-lifecycle.md) → evaluation guide. |
| Structured SQL and agents | [Routing, agents, and structured query](routing-agents-and-structured-query.md) → [Deterministic query handling reference](deterministic-sql-matcher.md) → [Ingestion, schema catalog, and Data Explorer](ingestion-schema-catalog-and-explorer.md) → evaluation guide. |
| Hospital-agnostic agents and schema binding | [Hospital agents and their data map](hospital-agents-and-data-map.md) → [Ingestion, schema catalog, and Data Explorer](ingestion-schema-catalog-and-explorer.md) → [Routing, agents, and structured query](routing-agents-and-structured-query.md) → [Deterministic query handling reference](deterministic-sql-matcher.md) → [`plans/new/00-README.md`](../new/00-README.md). |
| Ingestion and source metadata | [Ingestion, schema catalog, and Data Explorer](ingestion-schema-catalog-and-explorer.md) → [Architecture and data model](architecture-and-data-model.md) → [Operations, performance, and observability](operations-performance-and-observability.md). |
| Desktop App, Mobile App, and Tauri bridge | [Application and feature architecture](application-and-feature-architecture.md) → [System Architecture Design](system-architecture-design.md) → [Security, authentication, and audit](security-auth-and-audit.md). |
| Security and compliance | [Security, authentication, and audit](security-auth-and-audit.md) → [Architecture and data model](architecture-and-data-model.md) → [Verified platform notes](verified-platform-notes.md) → release guide. |
| Operations and release | [Operations, performance, and observability](operations-performance-and-observability.md) → [Evaluation, production validation, and release](evaluation-production-and-release.md) → [Verified platform notes](verified-platform-notes.md) → security guide. |

## Terminology and diagram policy

- Product clients must be called **Desktop App** and **Mobile App**. Tauri, the Rust bridge, the web view, React, Vite, and TypeScript are implementation details; never use “React UI” as a substitute for either product client.
- Database diagrams must group PostgreSQL, MySQL, and SQL Server under **External Clinical Databases**.
- Database diagrams must show **Internal DocumentDB Hybrid Store (documents + vectors + metadata/state)** separately from External Clinical Databases. DocumentDB is the application-owned hybrid persistence and search plane, not an operational clinical source.

## Status labels and evidence

- **Implemented** — behavior exists in the current repository and has been checked against code; the applicable guide may state narrower runtime evidence. It does not by itself prove production readiness on every host.
- **Partial** — a useful implementation exists, but an identified path, UI, validation, scale property, platform, or operational guarantee remains incomplete or unverified.
- **Planned** — intended behavior or accepted future direction that is not implemented. A plan, stub, test case, or runbook is not proof of completion.

The running implementation and tests are the final arbiter when documentation and code disagree. Dated runtime claims require the stronger evidence standard in [Verified platform notes](verified-platform-notes.md). Targets and procedures are not achieved results unless a committed artifact records the environment, revision, method, and outcome.

## Documentation maintenance rules

1. Verify behavior against current server, bridge, frontend, tests, and configuration before changing a status or architectural claim.
2. When behavior changes, update the affected authoritative guide in the same change. Update [Evaluation, production validation, and release](evaluation-production-and-release.md), its linked eval documentation/data, and [Verified platform notes](verified-platform-notes.md) when evidence, fixtures, gates, or verified platform behavior changes.
3. Update [Deterministic query handling reference](deterministic-sql-matcher.md) whenever matcher order, accepted frames, schema requirements, generated SQL, refusal rules, safety controls, or fixture coverage changes.
4. Do not copy phase checkboxes or status prose from historical plans. Re-establish status from code and evidence, and preserve important historical rationale as a clearly identified superseded decision.
5. Keep cross-boundary contracts synchronized. In particular, server response/event changes must be mirrored by the Tauri bridge and TypeScript bindings.
6. Keep all inference and health-record processing on premises. Do not document a cloud fallback that the architecture forbids.
7. Add new authoritative guides to this index, link them from related guides, and update the coverage matrix when a new historical source is consolidated.

## Consolidation and source coverage

The 32 Markdown files archived from the root of `plans/` into [`plans/old/`](../old/) are represented below. A row identifies the authoritative guide or guides that absorbed each source's enduring decisions; it does **not** make the historical file current authority.

| Authoritative destination | Historical source files represented |
| --- | --- |
| Architecture and core data model | [`00-master-plan.md`](../old/00-master-plan.md); [`02-verification-and-decode-findings.md`](../old/02-verification-and-decode-findings.md); [`03-ws6-retrieval-chat-implementation.md`](../old/03-ws6-retrieval-chat-implementation.md) |
| Application and feature architecture | [`UI redo plan.md`](../old/UI%20redo%20plan.md); [`08-data-explorer-implementation.md`](../old/08-data-explorer-implementation.md); [`09-chat-implementation.md`](../old/09-chat-implementation.md); [`10-agents-implementation.md`](../old/10-agents-implementation.md); [`13-profile-admin-implementation.md`](../old/13-profile-admin-implementation.md); [`15-stage-11-polish-android.md`](../old/15-stage-11-polish-android.md); [`25-concurrent-chat-orchestration.md`](../old/25-concurrent-chat-orchestration.md) |
| Ingestion, schema catalog, and explorer | [`07-ingest-wizard-implementation.md`](../old/07-ingest-wizard-implementation.md); [`25-ingestion-extractor-and-verifier.md`](../old/25-ingestion-extractor-and-verifier.md); [`26-schema-metadata-catalog.md`](../old/26-schema-metadata-catalog.md); [`Sol-Findings.md`](../old/Sol-Findings.md); [`Sol-implementation plan.md`](../old/Sol-implementation%20plan.md) |
| Retrieval, memory, and concurrency | [`01-retrieval-design.md`](../old/01-retrieval-design.md); [`19-retrieval-and-faithfulness.md`](../old/19-retrieval-and-faithfulness.md); [`22-chat-memory-and-compaction.md`](../old/22-chat-memory-and-compaction.md) |
| Routing, agents, and structured SQL | [`06-agents-routing-and-aggregation.md`](../old/06-agents-routing-and-aggregation.md); [`17-intent-router-v2.md`](../old/17-intent-router-v2.md); [`18-text-to-sql-harness.md`](../old/18-text-to-sql-harness.md) |
| Models and accelerators | [`05-accelerator-detection.md`](../old/05-accelerator-detection.md); [`11-models-settings-implementation.md`](../old/11-models-settings-implementation.md); [`23-model-download-persistence.md`](../old/23-model-download-persistence.md) |
| Security, authentication, and audit | [`14-audit-log-implementation.md`](../old/14-audit-log-implementation.md); [`16-security-hardening.md`](../old/16-security-hardening.md) |
| Operations and performance | [`04-log-streaming.md`](../old/04-log-streaming.md); [`20-performance-fast-path.md`](../old/20-performance-fast-path.md); [`23-performance-efficiency-roadmap.md`](../old/23-performance-efficiency-roadmap.md) |
| Evaluation, production, and release | [`21-eval-harness.md`](../old/21-eval-harness.md); [`24-production-validation-runbook.md`](../old/24-production-validation-runbook.md); [`24-versioning-and-updates.md`](../old/24-versioning-and-updates.md) |

Some sources legitimately inform multiple guides; the compact matrix assigns each filename once to make completeness auditable. The guides' own **Source plans consolidated** sections provide more detailed many-to-many provenance.

## Archive

Historical root plans are preserved in [`plans/old/`](../old/). The following superseded curated guides are preserved in [`plans/old/docs/`](../old/docs/) for provenance and historical context. They contain stale defaults, incomplete phase snapshots, or proposals superseded by the authoritative guides above. **Do not use them as current truth.**

| Superseded document | Use instead |
| --- | --- |
| [Architecture overview](../old/docs/architecture-overview.md) | [Architecture and data model](architecture-and-data-model.md) |
| [App feature surface and requirements](../old/docs/app-feature-surface-and-requirements.md) | [Application and feature architecture](application-and-feature-architecture.md) |
| [Retrieval pipeline](../old/docs/retrieval-pipeline.md) | [Retrieval, chat memory, and concurrency](retrieval-chat-memory-and-concurrency.md) |
| [Aggregation-aware retrieval](../old/docs/aggregation-aware-retrieval.md) | [Routing, agents, and structured query](routing-agents-and-structured-query.md) |
| [Model selection and routing](../old/docs/model-selection-and-routing.md) | [Models, accelerators, and lifecycle](models-accelerators-and-lifecycle.md) |
| [Accelerator and hardware detection](../old/docs/accelerator-and-hardware-detection.md) | [Models, accelerators, and lifecycle](models-accelerators-and-lifecycle.md) |
| [Security and auth architecture](../old/docs/security-and-auth-architecture.md) | [Security, authentication, and audit](security-auth-and-audit.md) |
| [Log streaming](../old/docs/log-streaming.md) | [Operations, performance, and observability](operations-performance-and-observability.md) |
| [Decode and verification notes](../old/docs/decode-and-verification-notes.md) | [Verified platform notes](verified-platform-notes.md) |

## Repository context

- `onprem-rag-server/` contains the Rust Rocket server and the authoritative runtime implementation.
- `onprem-rag-app/` contains the React client and Tauri bridge.
- `eval/` contains committed fixtures, the local evaluator, and dated benchmark notes.
- `perf/` contains load tooling; a script is not a completed capacity result.
- `.env.example` documents supported configuration; real secrets belong only in ignored local environment files.

Last reviewed: 2026-09-04.
