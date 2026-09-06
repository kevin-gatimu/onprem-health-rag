<!-- markdownlint-disable MD013 -->

# 08 — Evaluation, rollout and documentation

**Goal:** prove the system is hospital-agnostic and regression-free before each merge, define the
rollout order, and keep `plans/docs/` truthful.

**Depends on:** all previous plans (fixtures are written alongside each).

---

## 1. Second synthetic schema ("alt hospital")

`docker/dev-postgres-alt/` — a **different** naming convention and structure for the same domain,
seeded by a variant of `generate_seed.py` (`--profile alt`, same RNG seed for comparable counts):

| Dev seed | Alt schema | Structural twist |
|---|---|---|
| `patients` | `PatientMaster` (`MRN`, `GivenName`, `Surname`, `DOB`, `Sex`) | PascalCase, no `patient_no` |
| `encounters` | `Visit` (`VisitDate`, `VisitType`, `DeptCode` → `Dept`) | dept as code FK |
| `admissions` | `IPD_Admission` (`AdmitDT`, `DischDT`, no LOS column) | LOS must be computed |
| `appointments` | `Booking` (`Slot`, `BookingStatus`) | status vocabulary `DNA` instead of `no_show` |
| `deliveries`, `newborns` | `OB_Delivery`, `OB_Baby` | prefixes |
| `prescriptions` + `prescription_items` | `Rx`, `RxLine` | |
| `lab_orders`, `lab_results` | `LabReq`, `LabRes` (`AbnormalFlag` char instead of boolean) | predicate on char |
| `billing_encounters`, `payments` | `Invoice`, `Receipt` (`AmountUSD`) | currency in name |
| `providers`, `provider_shifts` | `Staff`, `Roster` | |
| `incident_reports` | `Incident` | |
| ~25 tables total, MySQL **or** Postgres via compose profile | | exercises the second dialect |

Purpose: plan 01 binder ≥ 22/25 concepts; plan 03 golden questions ≥ 50/60 deterministic; plan 05
agents usable ≥ 9/13 (some lines legitimately unusable — e.g. no `Facilities` tables — and the UI
must hide them).

## 2. Fixtures

| File | Shape | Used by |
|---|---|---|
| `eval/data/router.jsonl` (extend) | `+ service_line_expected, backend_expected, deterministic_expected, focus: {patient, time_range}?` | `--suite router` via `POST /route` (plan 02 §4) |
| `eval/data/sql_golden.jsonl` (new) | `{ id, question, schema: dev\|alt, shape, expect_rows?: n, expect_scalar?: v, expect_top_label?: s }` | `--suite sql` via `POST /nl2sql/<sid>` comparing to ground truth computed by `eval/truth.sql` per schema |
| `eval/data/agents_t_series.jsonl` (new) | `{ id, agent, mode, question, expect_backend, expect_provenance_contains, expect_scalar?/expect_rows? }` — 10 per Tier-1 agent, 5 per Tier-2/3 | `--suite agents` via `POST /agents/<kind>` SSE |
| `eval/data/conversation_flows.jsonl` (new) | `{ id, turns: [{ q, expect_route, expect_deterministic, expect_focus_used?, expect_clarify_slot? }] }` | `--suite flows` (creates a conversation, replays turns) |
| `eval/data/retrieval.jsonl` (extend) | `+ filter_tables?, expect_excluded_tables?` | `--suite retrieval` |
| `onprem-rag-server/src/ontology/tests/{dev,alt}_seed_binding.json` | table → concept/lines | `cargo test` |
| `onprem-rag-server/src/nl2sql/ir/tests/golden.jsonl` | question → shape + SQL per dialect | `cargo test` |

`eval/run.mjs` additions: `--suite sql|agents|flows`, `--schema dev|alt`, `--agent <slug>`,
SSE consumer that captures `routed`, `provenance`, `sql`, `rows`, `suggestions`, `done`; report
includes per-rung latency percentiles and Tier-2 invocation rate.

## 3. Gates (per PR, in order)

1. `cargo check && cargo test --bin onprem-server` (server), `cargo check` (bridge), `npx tsc --noEmit` (app).
2. CI grep: no dev-seed table names in `src/ontology/**`, `src/nl2sql/ir/**`, `src/router/**`, `src/agents/**` outside `tests/` and fixtures.
3. Live (dev seed up): `node eval/run.mjs --suite full --smoke` — router ≥ 0.95, hit-rate@6 = 1.0, A-series 10/10.
4. Live (alt seed up, `--schema alt`): binder ≥ 22/25; sql golden ≥ 50/60; agents ≥ 9 usable.
5. Flows: all conversation flows pass with zero model calls in resolution steps (assert via `provenance.elapsed_ms` lacking `rewrite`/`classify` keys where `expect_deterministic`).
6. Latency budget on the dev host: deterministic p50 < 300 ms, model-planned p50 < 8 s, Tier-2 rate ≤ 25 %.
7. No new network destinations (grep `reqwest::`/`http://`/`https://` in server diff → only localhost/Foundry SDK).

## 4. Rollout order and flags

| Step | Merge | Flag(s) default | Notes |
|---|---|---|---|
| 1 | Plan 01 | `ONPREM_BINDING_ENABLED=true` | Additive; nothing consumes it yet except `GET /agents` and Settings. |
| 2 | Plan 03 behind `ONPREM_SQL_IR_ENABLED=false` | off | Dual-run: old templates answer, IR result logged + compared (`ir_shadow` metric). Flip on when `wrong == 0` on both the dev and alt bindings (62 rows/binding, 124 total — a single wrong row blocks the flip regardless of `verified`) and `verified` is at or above the ratchet floor asserted in `nl2sql/ir/mod.rs`, not a number restated here. The old `passed >= 55/60` gate is gone: it only required compilation not to error, which is exactly how 52 wrong-SQL rows got certified as passing. |
| 3 | Plan 02 with `ONPREM_ROUTER_V3=false` | off | Shadow route logged next to v2 decision; flip when accuracy ≥ 0.95 on the extended fixture. |
| 4 | Plan 04 | — | Executor replaces inline ladders once 02/03 are on; delete legacy paths in the same PR. |
| 5 | Plan 05 | legacy kinds mapped | Old UI keeps working through the mapping. |
| 6 | Plan 06 | `ONPREM_FOCUS_ENABLED=true`, suggestions on | |
| 7 | Plan 07 | — | Ship the UI; remove legacy kind mapping one release later. |
| 8 | Delete `nl2sql/routes.rs` template families, `ONPREM_SQL_IR_ENABLED`/`ONPREM_ROUTER_V3` flags | — | After two green eval cycles. |

Each step is one PR (or a short stack) with its own gates above; no step merges with a red gate.

## 5. Documentation updates (in `plans/docs/`, same PR as the code)

| Doc | Change |
|---|---|
| `hospital-agents-and-data-map.md` | Status → Implemented; §4 physical table map becomes "dev-seed binding example"; add §8 "Binding to other schemas" pointing at plan 01 semantics. |
| `routing-agents-and-structured-query.md` | Router v3 tiers, `RouteDecision` fields, backend selection, clarify; executor ladder; provenance event. |
| `deterministic-sql-matcher.md` | Rewrite around `QuerySpec` grammar/predicates/compiler; template inventory becomes the golden suite reference. |
| `retrieval-chat-memory-and-concurrency.md` | `RetrievalFilter`, `ConversationFocus`, suggestions. |
| `agentic-patterns.md` | Hybrid → Implemented; clarify pattern; capability route. |
| `application-and-feature-architecture.md` | Registry-driven tabs, modes, scope panel, provenance strip. |
| `evaluation-production-and-release.md` | New suites and gates; alt schema. |
| `README.md` (plans/docs) | Index entries; note that `plans/new/` are the in-flight plans and move them to `plans/old/` when Implemented. |
| Root `CLAUDE.md` | Stack line for routing: "tiered router v3 → StructuredExecutor ladder; agents = service lines bound per source". |

## 6. Operational notes for implementers

- Real secrets live in root `.env`; never create `onprem-rag-server/.env` (repo memory).
- DocumentDB `cosmosSearch` `filter` support must be verified live before plan 04 §4 is finalised; keep the over-fetch fallback.
- `cargo run` may fail under Smart App Control (os error 4551); use check/test and the already-running server for live checks.
- Keep `qwen3-8b` as default; `ONPREM_NL2SQL_PLAN_TIMEOUT_SECS=90` remains in root `.env` for the model-planned fallback.
- Foundry LRU unload/reload (40 s+) is why deterministic-first matters; measure Tier-2 rate in every eval report.
