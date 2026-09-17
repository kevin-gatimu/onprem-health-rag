<!-- markdownlint-disable MD013 -->

# plans/new — Hospital-agnostic agents, router v3, schema-driven SQL

Implementation plans for the next major iteration of the on-prem RAG system. Written against the
code as of 2026-09-04 (see [Baseline](#baseline-what-exists-today)). Every plan lists exact files,
types, endpoints, config keys, tests and acceptance gates so it can be implemented without
re-deriving context.

## Goals (from the product brief)

1. **A router that identifies intent and drives this workflow exactly:**

   ```mermaid
   flowchart TD
       Q[User question] --> R[Tiered local router]
       R -->|conversation| C[Local conversational answer]
       R -->|exact or structured| L[Link one external source]
       L --> D[Deterministic SQL templates]
       D -->|miss| P[Foundry Local SQL proposal]
       D --> V[AST validation + allowlists + limits]
       P --> V
       V --> X[Guarded read-only execution]
       X --> SR[Exact rows + SQL provenance]
       V -->|bounded failure| A[Validated internal aggregation]
       A -->|failure| S[Semantic RAG]
       R -->|semantic| S
   ```

2. **Fluid conversation** — the assistant knows what "she", "that ward", "the same period" refer
   to, carries a focus entity across turns, asks a clarifying question when it must, and offers
   next-step suggestions.
3. **Deterministic NL→SQL that is schema-driven**, able to build counts, groupings, joins, time
   buckets, rankings, filters, listings and lookups on *any* relational hospital schema — not the
   dev seed's table names.
4. **Hospital service-line agents** (Ask, Patient Chart, Front Desk, Ward Board, Emergency,
   Maternity, Theatre, Pharmacy, Diagnostics, Revenue, Quality & Safety, Workforce, Chronic Care,
   Facilities + the Service Trends / Handover modes) per
   [hospital-agents-and-data-map.md](../docs/hospital-agents-and-data-map.md).
5. **Works with most hospitals' databases**: the agent roster is a fixed *hospital ontology*; which
   physical tables and columns each agent owns is *bound per source at catalog time* (heuristics +
   embeddings + admin overrides), never hardcoded.

## Plan index and build order

| # | Plan | Depends on | Delivers |
|---|---|---|---|
| 01 | [Service-line ontology and schema binding](01-service-line-ontology-and-schema-binding.md) | — | `ServiceLine`, `ColumnRole`, `SchemaBinding` per source; binder; overrides; `GET /agents` capability API |
| 02 | [Intent router v3](02-intent-router-v3.md) | 01 | Backend-aware routing, entity/service-line extraction, follow-up + clarification, `Hybrid` executor decision |
| 03 | [Schema-driven deterministic SQL](03-schema-driven-deterministic-sql.md) | 01 | `QuerySpec` IR, grammar→IR matcher, IR→dialect compiler, replaces hardcoded templates |
| 04 | [Structured execution and fallback ladder](04-structured-execution-and-fallbacks.md) | 02, 03 | Single `StructuredExecutor` implementing the flowchart, provenance events, scoped aggregation, retrieval table filter, hybrid cohort |
| 05 | [Hospital agents (server)](05-hospital-agents-server.md) | 01, 04 | New `AgentKind`s, personas from binding, scoped linking/aggregation/retrieval, Trends/Handover modes |
| 06 | [Conversation memory and suggestions](06-conversation-memory-and-suggestions.md) | 02, 04 | `ConversationFocus` state, anaphora resolution, `suggestions`/`clarify` SSE events |
| 07 | [App restructure](07-app-restructure.md) | 05, 06 | Server-driven agent registry, tabs by tier, scope panel, suggestion chips, SQL provenance, draft continuity |
| 08 | [Evaluation, rollout, docs](08-evaluation-and-rollout.md) | all | Second synthetic schema, per-agent fixtures, gates, rollout sequence, doc updates |

Implement in numeric order. 01 and 03 can be developed in parallel branches (03 needs only the
`ColumnRole` type from 01). 06 and 07 can be developed in parallel once 04/05 are merged.

## Implementation briefs and corrections

Numbered plans state *what* to build. Lettered briefs attached to a plan state *how*, or correct a
defect found while building it. A brief binds the plan it hangs off.

A brief describes the state of the code at the time it was written; once implemented, its
entry-state descriptions and line numbers are history and are not maintained against the tree —
cite identifiers, not line numbers, when following one.

| # | Brief | Applies to | Purpose |
|---|---|---|---|
| 01a | [Implementation brief](01a-implementation-brief.md) | 01 | Build guidance for the ontology and binder |
| 02a | [Implementation brief](02a-implementation-brief.md) | 02 | Build guidance for router v3 |
| 03a | [Implementation brief](03a-implementation-brief.md) | 03 | Build guidance for the IR; section 0.6 constrains join forms |
| 03b | [Defect closure brief](03b-defect-closure-brief.md) | 03 | Governing invariant plus the non-negotiables every later pass inherits |
| 03c | [Enum fidelity and criticality](03c-enum-fidelity-and-criticality.md) | 03 | Real enum labels into the harness; criticality as a role distinct from abnormality |
| 03d | [Fixture drift audit](03d-fixture-drift-audit.md) | 03 | Evidence: 18 of 28 blessed dev rows referenced tables or columns that do not exist |
| 03e | [Fixture from ground truth](03e-fixture-from-ground-truth.md) | 03 | Derive the dev fixture from the real DDL so it cannot drift again |
| 03f | [Scoped reverse FK](03f-scoped-reverse-fk.md) | 03 | Amends 03a section 0.6 to permit a single scoped reverse hop; closes the cohort family |
| 03g | [Derived temporal measures](03g-derived-temporal-measures.md) | 03 | Length of stay and turnaround from timestamp differences; normalises dialect divergence |
| 03h | [Harness invariants](03h-harness-invariants.md) | 03 | Converts the audit heuristics that caught the defects into assertions, so the classes cannot silently return |
| 04a | [Acceptance measure](04a-acceptance-measure.md) | 04 | Amends 04 sections 8 and 9; executed-result equivalence and cross-rung agreement |

Order within plan 03: 03a, 03b, 03c, 03d, 03e, 03f, 03g, 03h. The last four are strictly serial —
03e, 03f and 03g all edit `golden.jsonl`, and 03h asserts over the emitted SQL of every row, so it
runs last against the final fixture and the final rule set. 04a must exist and be reviewed before
plan 04 begins.

## Non-negotiable invariants (unchanged)

- PHI never leaves the premises; no cloud model or service call anywhere in these plans.
- Server is the policy boundary; the bridge holds the JWT; the web layer never does.
- Models **propose**, Rust **decides**: every SQL, aggregation spec and route is validated by code.
- Fail open to grounded semantic retrieval; never to an error when a truthful answer exists.
- Ownership maps are read allow-lists, not security boundaries (RBAC remains in `auth/`).
- Every new server response field is mirrored in `src-tauri/src/commands.rs` and `src/lib/bridge.ts`.
- Streamed tokens are JSON-encoded in SSE `data:`.

## Baseline: what exists today

Verified against source on 2026-09-04. Plans reference these symbols by name.

**Server (`onprem-rag-server/src`)**

- `router/mod.rs` (771 lines) — `RouteClass { Conversational, ConversationMeta, Structured{intent, backend}, Semantic, Hybrid{cohort_intent} }`, `StructuredBackend { DocDb, SourceSql }`, `RouteDecision { class, tier, cached, entities: Option<RouteEntities> }`, `RouteEntities { tables, metric, time_bucket }` (carried but unused), Tier 0 regex gate (`router/conversational.rs`), Tier 1 lexical (`aggregation/intent.rs::classify_lexical`), Tier 2 tool-call classifier with LRU `RouterCache`.
- `aggregation/intent.rs` — `QueryIntent { Lookup, Narrative, Aggregation, Trend, Enumeration, MultiHop }`.
- `nl2sql/routes.rs` (2906 lines) — `prepare_auto_query`, `prepare_auto_query_deterministic`, `try_deterministic`, `deterministic_sql` (hardcoded to dev table names: `common_healthcare_sql`, `top_grouped_count_join_sql`, `recent_records_subject`, `periodic_trend_subject`, `patient_overview_subject`, `relationship_filtered_count_sql`, `grouped_count_sql`, `patient_gender_listing_sql`), `PreparedNlQuery`, `MetadataOverrides { aliases, relationships }` with `GET/PUT /nl2sql/<sid>/catalog/overrides`, planner + one repair in `prepare_with_cards`.
- `nl2sql/spec.rs` — `TableCard { source_id, table_name, row_count, columns: Vec<CardColumn>, fk_edges, card_vector, card_text }`, `CardColumn { name, type_, nullable, is_primary_key, is_foreign_key, sample_values, profile: ColumnProfile }`.
- `nl2sql/catalog.rs` — `refresh_catalog*`, `build_profiles`, `structural_fingerprint`, drift poller, `schema_catalog` / `schema_catalog_state` / `schema_catalog_history` / `schema_metadata_overrides` collections.
- `nl2sql/linker.rs` — `link`, `link_best_source`, BGE-M3 card ranking + explicit-mention boost + one-hop FK expansion.
- `nl2sql/validate.rs` — `validate_sql(sql, dialect, max_rows, allowed_tables) -> ValidatedSql` via `sqlparser`; LIMIT/TOP injection; blocked functions.
- `nl2sql/execute.rs` — `run_select(db, config, source_id, sql) -> (columns, rows)` with optional cost pre-flight.
- `aggregation/` — `RunAggregation`, `RunList`, `Catalog { collections, code_vocab, synonyms }`, `build_from_store`, validate, DocumentDB pipeline execute.
- `answer.rs` — `run_structured(...) -> Structured { Direct, Aggregate, List }`, `NARRATION_SYSTEM_PROMPT`.
- `agents/routes.rs` (1121 lines) — `POST /agents/<kind>` dispatcher for `auto | health_query | trends | patient_lookup | summarize | chat`, `should_try_deterministic_sql`, `class_to_agent_kind`.
- `rag/routes.rs` (1116 lines) — `POST /route`, `POST /search`, `POST /chat`; `ChatData { Conversational, DirectStructured, Structured, SourceSql, Semantic… }`; `resolve_followup_sql`, `resolve_semantic_sql`.
- `foundry/router.rs` — `AgentKind { PatientLookup, HealthQuery, Trends, Summarize, Chat, MultiHop, QueryRewrite, Classify, Extract, Verify, TextToSql }`, `ModelSpec::for_kind`.
- `memory.rs` — `WorkingMemory { summary, tail }`, `load_working_memory`, `maybe_spawn_compaction`.
- `retrieval/mod.rs` — `retrieve_observed(db, config, queries, mode, rerank, top_k, trace)`; **no table filter**.
- `connectors/` — `SourceConnector { test, get_schema, count_table, fetch_table_page, estimate_cost, run_select }`.

**App (`onprem-rag-app`)**

- `src-tauri/src/commands.rs` — `chat`, `agent(kind, question, conversation_id, run_id)`, `list_agent_conversations`, conversation CRUD; mirror structs `StoredMessage { …, agent_kind, structured, sql_result }`, `StructuredResult`, `SqlResult`.
- `src/lib/bridge.ts` — `AgentKind = "auto" | "health_query" | "trends" | "patient_lookup" | "summarize" | "chat"`; `src/lib/bridgeEvents.ts` single boot listener; envelope events `chat://*`, `agent://*`.
- `src/features/agents/index.tsx` — `KIND_TABS`, `KIND_BLURBS` hardcoded; `auto` renders `<Chat/>`.
- `src/stores/agents.ts` — `AgentPending`, `STRUCTURED_KINDS`, per-kind drafts; `setSelectedKind` resets `activeConversationId`.

**Data** — `docker/dev-postgres/init/*.sql`: 64 tables (inventory in plan 01 §7).

**Eval** — `eval/run.mjs` (`--suite router|retrieval|full`), `eval/data/router.jsonl`, `retrieval.jsonl`, `QUESTIONS.md` A-series 10/10 pass via deterministic SQL.

## Workflow assessment: where the flowchart is right and where it needs adjustment

The flowchart is sound. Implementing it *literally* exposes six gaps, each addressed in a plan:

| Gap | Today | Fix | Plan |
|---|---|---|---|
| `backend` is chosen by Tier-1/2 string heuristics and `RouteEntities` are discarded | `StructuredBackend::SourceSql` picked by lexical shape only | Router emits `ServiceLine` + entities + backend from *binding coverage*; SourceSql only when a connected source has bound tables for the detected service line | 02 |
| "Deterministic SQL templates" are schema-specific | 19 template families hardcoded to `patients`, `encounters`… | Grammar → `QuerySpec` IR → compile against `SchemaBinding` roles | 03 |
| "Link one external source" ignores agent scope | linker ranks all cards | Linker filters to `ServiceLine` allow-list before ranking | 01, 05 |
| "Bounded failure → internal aggregation" is implicit and per-call-site | fallback logic duplicated in `rag/routes.rs`, `agents/routes.rs` | One `StructuredExecutor::run()` state machine, reused by `/chat` and `/agents/*`; emits a `provenance` event with the *path actually taken* | 04 |
| Conversation branch is a dead end | `Conversational` → canned reply, no memory of what came before | `ConversationFocus` (entities, time range, last result) feeds every branch; `clarify` branch added | 06 |
| Semantic RAG cannot be scoped | no table filter | `RetrievalFilter { tables, source_ids, patient_key }` applied in `$match` before `cosmosSearch`/`$text` | 04 |

Additional improvements recommended (all bounded, all local):

- **Clarify branch.** When the router is confident it is structured but the `QuerySpec` has an unbound required slot (e.g. "how many were cancelled?" with no subject and no focus), ask *one* clarifying question instead of guessing. Bounded: at most one clarification per turn, never two in a row.
- **Deterministic-first everywhere.** Run the IR matcher *before* the Tier-2 model classifier: a successful IR parse *is* a routing decision (structured, SourceSql) and saves 2–4 s.
- **Provenance is a first-class SSE event**, not inferred by the client from which events arrived.
- **Hybrid** becomes real: structured cohort → retrieval filter → grounded synthesis.

## Gates shared by all plans

- `cargo check` + `cargo test --bin onprem-server` (server); `cargo check` in `src-tauri`; `npx tsc --noEmit` in app.
- New DocumentDB syntax verified live against the `documentdb` container before merge (VERIFY-EARLY).
- Eval: `node eval/run.mjs --suite full --smoke` router accuracy ≥ 0.95, hit-rate@6 = 1.0; A-series 10/10 (regression guard for plan 03).
- No plan may introduce a network call to a non-localhost host.
