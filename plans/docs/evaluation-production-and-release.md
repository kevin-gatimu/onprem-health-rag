# Evaluation, Production Validation, and Release

**Authoritative status:** repository-verified on 2026-09-02. A committed runbook or target is not treated as a completed validation result.

## What exists today

### Automated deterministic evaluation

[`eval/run.mjs`](../../eval/run.mjs) is a local Node runner that authenticates to the on-prem server and supports:

- `router`, `retrieval`, and combined `full` suites;
- `--smoke` selection of the first five fixtures;
- retrieval presets `full`, `naive`, `no-rerank`, and `fast`;
- router route/intent/tier comparison;
- retrieval hit-rate@6 and MRR from expected record-id fragments;
- configurable thresholds and request timeout; and
- timestamped JSON reports under ignored `eval/reports/`.

Its committed data is small and synthetic:

| Dataset | Current size and scope |
| --- | --- |
| [`router.jsonl`](../../eval/data/router.jsonl) | 13 deterministic routing cases across conversational, structured, and semantic decisions. |
| [`retrieval.jsonl`](../../eval/data/retrieval.jsonl) | 7 medication/allergen retrieval cases against seeded catalog rows. |

`--validate-only` parses both files without contacting a server. The runner sends requests only to a configured local/on-prem endpoint; fixtures must never contain production PHI.

### CI

[`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) runs on push and pull request using Windows runners. It currently:

1. checks and tests `onprem-rag-server`;
2. installs app dependencies with pnpm;
3. runs the TypeScript check and Vite production build; and
4. runs `node eval/run.mjs --validate-only`.

CI proves fixture syntax, compilation, and server unit tests. It does **not** start DocumentDB/Foundry/dev sources or execute the live retrieval/router smoke suite. It also does not build the Tauri Rust bridge or Android package in the committed workflow.

### Manual end-to-end benchmark

[`eval/AUTO_BENCHMARK.md`](../../eval/AUTO_BENCHMARK.md) records a dated 2026-09-01/02 synthetic-data UI run. Its current headline result is 10/10 strict passes across the A-series, with 11 measured requests and no timeouts, after fixing the A10 follow-up grounding defect. It also records newer S-series join cases separately: S06 and S07 passed, while S08/S09 remained unrun at the time of the note. [`eval/QUESTIONS.md`](../../eval/QUESTIONS.md) records semantic case S01 as a failure and several semantic/dedicated-tab cases as planned.

These are useful regression facts for the stated server build and environment. They are not a general performance/capacity certification, and the benchmark itself notes that all final A-series passes used deterministic SQL rather than the local model planner.

## Evaluation coverage status

| Capability | Status |
| --- | --- |
| Router accuracy | **Implemented, limited dataset.** Automated against 13 synthetic fixtures. |
| Retrieval hit-rate/MRR | **Implemented, limited dataset.** Automated against 7 seeded catalog fixtures. |
| Deterministic final-answer SQL | **Manual/E2E evidence exists.** A-series passed on 2026-09-01/02; some S-series cases were added later. Not yet part of `eval/run.mjs`. |
| Structured exact-result automation | **Planned/partial.** SQL logic has unit tests and a manual benchmark, but no committed `run.mjs` structured suite. |
| No-answer/refusal precision and recall | **Planned.** No automated dataset/report calculates these metrics. |
| Citation correctness and grounded generation | **Partial.** Runtime verification exists when enabled, but no complete committed evaluation slice establishes release thresholds. |
| Local judged RAGAs | **Not implemented in this repository.** The old project is prior art only; there is no committed Python/RAGAs scoring harness here. |
| Local LLM judge | **Not implemented.** Any future judge must remain on premises through Foundry Local. |
| Performance regression gate | **Partial.** Request telemetry and a load script exist, but CI has no live latency threshold gate. |
| Android E2E | **Open.** No committed production-device build/run, network, lifecycle, hardware-back, secure-storage, or release-install validation result. |

## Planned evaluation suites, gates, and rollout

**Status: Planned — see [plans/new/08-evaluation-and-rollout.md](../new/08-evaluation-and-rollout.md).** Nothing in this section exists in `eval/` today; it is the acceptance apparatus for the hospital-agnostic agents, router v3, and schema-driven SQL workstream.

### Alt-hospital schema

A second synthetic seed (`docker/dev-postgres-alt/`, ~25 tables, MySQL or Postgres by compose profile) uses different naming and structure for the same domain (`PatientMaster`/`MRN`, `Visit`, `IPD_Admission` without an LOS column, `Booking` with `DNA` status, `OB_Delivery`, `Rx`/`RxLine`, `LabReq`/`LabRes` with a char abnormal flag, `Invoice`/`Receipt`, `Staff`/`Roster`). It exists to prove the binder, IR compiler, and agents are not overfitted to the dev seed.

### Suites and fixtures

`eval/run.mjs` gains `--suite sql|agents|flows`, `--schema dev|alt`, `--agent <slug>`, an SSE consumer that captures `routed`, `provenance`, `sql`, `rows`, `suggestions`, `done`, and a report with per-rung latency percentiles and the Tier-2 invocation rate.

| Fixture | Shape | Suite |
| --- | --- | --- |
| `eval/data/router.jsonl` (extended) | `+ service_line_expected, backend_expected, deterministic_expected, focus?` | `router` via `POST /route` |
| `eval/data/sql_golden.jsonl` (new) | `{ id, question, schema, shape, expect_rows? \| expect_scalar? \| expect_top_label? }` against `eval/truth.sql` per schema | `sql` via `POST /nl2sql/<sid>` |
| `eval/data/agents_t_series.jsonl` (new) | 10 questions per Tier-1 agent, 5 per Tier-2/3, with `expect_backend`, `expect_provenance_contains` | `agents` via `POST /agents/<kind>` |
| `eval/data/conversation_flows.jsonl` (new) | multi-turn `{ q, expect_route, expect_deterministic, expect_focus_used?, expect_clarify_slot? }` | `flows` (creates a conversation, replays turns) |
| `eval/data/retrieval.jsonl` (extended) | `+ filter_tables?, expect_excluded_tables?` | `retrieval` |
| `ontology/tests/{dev,alt}_seed_binding.json`, `nl2sql/ir/tests/golden.jsonl` | table → concept/lines; question → shape + SQL per dialect | `cargo test` |

### Gates per PR (plan 08 §3, in order)

1. `cargo check && cargo test --bin onprem-server`; `cargo check` in `src-tauri`; `npx tsc --noEmit`.
2. CI grep: no dev-seed table names in `src/ontology/**`, `src/nl2sql/ir/**`, `src/router/**`, `src/agents/**` outside tests and fixtures.
3. Live, dev seed: `--suite full --smoke` — router ≥ 0.95, hit-rate@6 = 1.0, A-series 10/10.
4. Live, alt seed (`--schema alt`): binder ≥ 22/25 concepts; SQL golden ≥ 50/60; ≥ 9/13 agents usable.
5. Flows pass with zero model calls in resolution steps (asserted from `provenance.elapsed_ms` lacking `rewrite`/`classify` where `expect_deterministic`).
6. Latency on the dev host: deterministic p50 < 300 ms; model-planned p50 < 8 s; Tier-2 rate ≤ 25 %.
7. No new network destinations (server diff grep for `reqwest::`/`http://`/`https://` → only localhost / Foundry SDK).

### Feature-flag rollout (plan 08 §4)

| Step | Merge | Flag default | Notes |
| --- | --- | --- | --- |
| 1 | Plan 01 binding | `ONPREM_BINDING_ENABLED=true` | Additive; consumed only by `GET /agents` and Settings |
| 2 | Plan 03 IR | `ONPREM_SQL_IR_ENABLED=false` | Shadow run; old templates answer, IR compared (`ir_shadow` metric); flip when golden ≥ 55/60 |
| 3 | Plan 02 router v3 | `ONPREM_ROUTER_V3=false` | Shadow route logged beside v2; flip at accuracy ≥ 0.95 |
| 4 | Plan 04 executor | — | Replaces inline ladders once 02/03 are on; legacy paths deleted in the same PR |
| 5 | Plan 05 agents | legacy kinds mapped | Old UI keeps working through the mapping |
| 6 | Plan 06 focus/suggestions | `ONPREM_FOCUS_ENABLED=true` | |
| 7 | Plan 07 app | — | Remove legacy kind mapping one release later |
| 8 | Delete template families and the two shadow flags | — | After two green eval cycles |

Each step is one PR (or short stack) with its own gates; no step merges with a red gate. Documentation in `plans/docs/` is updated in the same PR (plan 08 §5).

## Production validation

The procedure in [the production validation runbook](../old/24-production-validation-runbook.md) covers preflight, 10/25/50 concurrency ladders, ingestion interruption/recovery, an eight-hour soak, and dependency/key-rotation drills. [`perf/load.mjs`](../../perf/load.mjs) can issue concurrent `/chat` requests and report status counts and p50/p95/p99 response latency.

**No committed report currently proves:**

- a completed 10/25/50-client ladder on named hardware;
- bounded RSS, queue depth, and task count after load;
- an eight-hour soak;
- recovery across every listed failure injection;
- multi-million-row or larger-than-RAM ingestion behavior;
- safe maximum concurrent generations, searches, or ingestions;
- production IVF recall/latency tuning or HNSW viability; or
- backup restoration and disaster-recovery drills.

Therefore the plan's TTFT, error-rate, ingestion, and soak values are release targets, not achieved SLOs. A production readiness decision must attach measured artifacts containing commit, hardware, execution providers, model variants, corpus size, configuration, concurrency, errors, latency percentiles, peak RSS, and quality results.

## Release state

### Version sources

The app package, Tauri crate, and `tauri.conf.json` currently report version `0.1.0`; the server crate also reports `0.1.0`. The intended app source of truth is `onprem-rag-app/src-tauri/tauri.conf.json`; the server source is `onprem-rag-server/Cargo.toml`. The Settings surface reads app/server versions for display.

### Updater

The updater is **not implemented**. There is currently no:

- updater dependency or capability;
- signing public key or `createUpdaterArtifacts` setting;
- Rocket `/updates/latest.json` route or update artifact service;
- bridge update command; or
- launch/settings update workflow.

The agreed future design is desktop-only, server-hosted, signature-verified, and user-confirmed rather than auto-installed. Android delivery is expected through signed APK/MDM processes, not the desktop updater. Until signing-key custody, artifact hosting, rollback, and release procedures are implemented and tested, updates are manual.

### Android release readiness

The Android project is initialized and explicitly permits runtime LAN HTTP. That enables development connectivity but is not a production security validation. Production device work still includes:

- build and install with the intended SDK/NDK and signing identity;
- physical-device connectivity and TLS-host configuration;
- background/foreground and process-death behavior;
- Android Keystore-backed token storage;
- hardware-back behavior;
- long SSE stream and cancellation behavior;
- notification permissions and delivery; and
- upgrade/rollback through the chosen MDM or sideload process.

No Android production-device validation result is committed, so Android must remain an open release gate.

## Minimum release evidence

For a release candidate using synthetic staging data:

1. Run server checks/tests, bridge check, frontend type-check/build, and deterministic fixture validation.
2. Execute live router and retrieval suites; archive JSON reports and `/metrics/summary`.
3. Run the exact-result Auto benchmark, including A10 follow-up and the currently relevant S/T cases.
4. Run refusal, citation, and semantic retrieval cases; do not accept deterministic SQL success as coverage for semantic RAG.
5. Execute the load ladder and at least the dependency failures relevant to the deployment.
6. Complete Android device validation if Android is in release scope.
7. Record all versions, hardware, models/EPs, corpus/configuration, security posture, and known exceptions.
8. Confirm no production PHI, credentials, prompts, or raw records entered CI artifacts, eval fixtures, logs, or reports.

A release should fail if required evidence is missing, not silently reinterpret “not run” as “pass.”

## Source plans consolidated

- [15 — Polish and Android](../old/15-stage-11-polish-android.md): initialized Android surface and explicitly deferred production device/hardware-back validation.
- [21 — Evaluation Harness](../old/21-eval-harness.md): deterministic router/retrieval runner and instrumentation are partly implemented; the larger dataset and local judged RAGAs remain incomplete.
- [23 — Performance/Efficiency Roadmap](../old/23-performance-efficiency-roadmap.md): provides target quality/performance gates, not measured production guarantees.
- [24 — Production Validation Runbook](../old/24-production-validation-runbook.md): actionable procedure exists, but no committed capacity/soak report proves completion.
- [24 — Versioning and Updates](../old/24-versioning-and-updates.md): metadata cleanup exists; updater implementation remains entirely open.
- [Sol Findings](../old/Sol-Findings.md) and [Sol implementation plan](../old/Sol-implementation%20plan.md): duplicate roadmap claims consolidated; stale “no CI” and “no evaluator” statements corrected.
