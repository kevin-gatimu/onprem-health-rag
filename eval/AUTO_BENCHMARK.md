# Auto feature benchmark

This benchmark evaluates the AI Agents **Auto** tab end to end against the deterministic synthetic
PostgreSQL facility data. It measures the final answer shown to a user, complementing
`eval/run.mjs`, which currently measures routing and retrieval only.

All tests must remain on premises. Never add production questions, records, credentials, or PHI to
this file or to benchmark reports.

## Current result

Run date: 2026-09-01/02  
Server build: 2026-09-01

| Metric | Result | Meaning |
| --- | ---: | --- |
| Executed cases | 10 / 10 | All cases ran through the app UI in Auto |
| Strict passes | 10 / 10 | Every final answer matched PostgreSQL after the A10 fix |
| Strict accuracy | 100% | Passes divided by executed cases |
| Coverage | 100% | Executed cases divided by planned cases |
| Measured requests | 11 / 11 | Ten initial prompts plus the A10 follow-up |
| Release status | **PASS** | Accuracy, coverage, safety, and timeout gates passed |

All final passing answers came from the deterministic NL→SQL compiler; no case used model
planning. **A10** exposed one faithfulness failure before the fix: its anaphoric follow-up took
73,056 ms on the semantic path and answered the wrong surname, "Jane Wairimu." The same-session
fix now grounds "the patient" to `SYN-2024-0001` from conversation memory and executes live SQL.
The re-run answered "Jane Chebet" in 299 ms, with the server logging
`anaphoric follow-up answered via deterministic SQL`.

## Environment

- UI: AI Agents > Auto, using the shared general Chat implementation
- App: React 19 / Vite frontend through the Tauri bridge
- Server: Rocket at `http://127.0.0.1:8000`
- Source: PostgreSQL 17.9, database `health_records`
- Store/model path: DocumentDB plus Foundry Local
- Data: deterministic synthetic Facility A seed only
- Browser QA: Playwright with a temporary Tauri IPC shim when run outside the Tauri shell

Before each benchmark run:

1. Start DocumentDB, the seeded PostgreSQL source, Foundry Local, the server, and the app.
2. Ingest the full seeded source and confirm that 60 patient records are indexed.
3. Confirm `GET /health` succeeds and no ingestion is active.
4. Use a fresh Auto conversation for every independent case.
5. Record the app/server revision, model, execution provider, and prompt budget in the run log.

## Scoring

### Case status

- **PASS**: all required values and records match the PostgreSQL oracle; no unsupported claim is
  present.
- **PARTIAL**: the answer is directionally correct but omits required rows/fields, mishandles a tie,
  or differs only in a permitted rounding tolerance.
- **FAIL**: a required fact is wrong, fabricated, refused despite available data, or the request
  errors/times out.
- **BLOCKED**: environment or test tooling prevented execution. Blocked cases are excluded from
  accuracy and included in the coverage denominator.
- **NOT RUN**: not attempted. Not-run cases are excluded from accuracy and included in coverage.

### Metrics

Use strict accuracy as the release metric:

```text
strict accuracy = PASS / (PASS + PARTIAL + FAIL)
coverage        = (PASS + PARTIAL + FAIL) / planned cases
```

For list questions, also calculate set quality using the `(patient_no, full_name)` pair:

```text
precision = correct returned rows / all returned rows
recall    = correct returned rows / expected rows
F1        = 2 * precision * recall / (precision + recall)
```

Measure two durations with a monotonic clock:

- **End-to-end latency**: question submission until the final answer is rendered. This is required
  for every executed case and is the user-perceived duration reported in the test matrix.
- **Time to first output (TTFO)**: question submission until the first visible token, SQL status, or
  structured result is rendered. This is optional until the harness captures it consistently.

Record milliseconds in raw reports and show seconds to two decimal places in Markdown. Record
timeouts as failures with the elapsed timeout value, not as missing observations. Summarize completed
runs with median (p50), p95, maximum, and the number of timed cases. Never derive percentiles from
cases whose duration was not captured.

### Release gates

- 100% benchmark coverage.
- At least 90% strict accuracy overall.
- 100% strict accuracy for patient-specific lookup and no-answer safety cases.
- List precision and recall both at least 95% for **A02** and **A03**.
- No fabricated patient, count, diagnosis, medication, or citation.
- No request failure, unhandled UI exception, or answer timeout over 180 seconds.
- Follow-up **A10** must preserve the immediately preceding result context.

## Test matrix

| ID | Type | Question | PostgreSQL oracle | Pass requirement | Status | Duration |
| --- | --- | --- | --- | --- | --- | ---: |
| A01 | Scalar count | How many patient records are indexed? | 60 patients | States 60 | PASS | 0.51 s |
| A02 | Filtered list | List the names and patient numbers of all female patients. | 35 exact rows | All and only 35 `(patient_no, name)` pairs | PASS | 0.36 s |
| A03 | Prefix filter | Show me patients whose first name starts with P. | 9 exact rows | All and only the 9 rows below | PASS | 4.55 s |
| A04 | Grouping | How many female and male patients are there? | Female 35; male 25 | Both labels and counts match | PASS | 0.25 s |
| A05 | Temporal peak | Which month had the most encounters, and how many? | August 2024; 10 | Month, year, and count match | PASS | 0.26 s |
| A06 | Top category | What is the most common diagnosis, and how many times does it occur? | Infectious gastroenteritis and colitis; 12 | Diagnosis and count match | PASS | 2.85 s |
| A07 | Numeric aggregate | What is the average length of stay for discharged admissions? | 82.44 days across 34 discharged admissions | Value within 0.01 and correct denominator | PASS | 2.84 s |
| A08 | Join and tie | Which medications were prescribed most often? | Artemether-Lumefantrine and Ibuprofen; 12 each | Returns both tied medications and count | PASS | 2.42 s |
| A09 | Boolean filter | How many lab results were abnormal? | 16 abnormal; 58 normal | States 16 abnormal | PASS | 0.19 s |
| A10 | Patient lookup + follow-up | Show encounter and diagnosis counts for patient SYN-2024-0001. Then ask: What is the patient's name? | 2 encounters; 2 diagnoses; Jane Chebet | First answer has both counts; follow-up names Jane Chebet | PASS | 4.62 s + 0.30 s |

| Timed cases | End-to-end p50 | End-to-end p95 | End-to-end max | Timeouts |
| ---: | ---: | ---: | ---: | ---: |
| 11 | 0.51 s | ≈ 4.6 s | 4.62 s | 0 |

The latency summary uses the final passing request measurements: 511, 356, 4,550, 254, 264,
2,848, 2,836, 2,421, 192, 4,618, and 299 ms. A10's first prompt measured 3,288 ms on its first
run and 4,618 ms on the final re-run; only the final passing run is included above.

### Planned deterministic grouped-count join cases

These cases extend deterministic SQL coverage without changing the current 10-case release result.
The expected path must compile from linked schema cards and FK edges without calling the local model
planner.

| ID | Question | Expected path | Required SQL relationship | Status |
| --- | --- | --- | --- | --- |
| S06 | Which providers ordered the most lab panels? | Deterministic top-N join | `lab_orders.ordered_by = providers.id` | PASS · 2026-09-02 · 10 rows match psql ground truth; execute ~470 ms (21.7 s total incl. cold tier-2 router warm-up); EXPLAIN cost 17.81 |
| S07 | Which providers prescribed the most medications? | Deterministic top-N join | `prescriptions.prescriber_id = providers.id` | PASS · 2026-09-02 · 3,976 ms (route 3.7 s); EXPLAIN cost 18.15 |
| S08 | Which providers had the most encounters? | Deterministic top-N join | `encounters.provider_id = providers.id` | NOT RUN |
| S09 | Which counties had the most admissions? | Local planner fallback | No supported grouped-count entity mapping | NOT RUN |

For S06–S08, require `COUNT(*)`, descending count order, a limit no greater than 10, and provider
labels derived from `providers.first_name` plus `providers.last_name`. S09 passes only when the
deterministic matcher declines it; the planner's eventual answer is scored separately.

### A03 expected rows

| Patient number | Name |
| --- | --- |
| SYN-2024-0017 | Patrick Omondi |
| SYN-2024-0026 | Paul Adhiambo |
| SYN-2024-0029 | Purity Wekesa |
| SYN-2024-0034 | Purity Kipchoge |
| SYN-2024-0040 | Patrick Wanjiru |
| SYN-2024-0044 | Paul Simiyu |
| SYN-2024-0045 | Patrick Omar |
| SYN-2024-0049 | Peter Atieno |
| SYN-2024-0052 | Patrick Wekesa |

The prefix is deliberately stated as **first name**. In this seed, no surname starts with P, so the
shorter question "show me patients starting with P" is ambiguous and should be a separate language
understanding test, not an exact-result test.

## PostgreSQL oracles

Run these read-only queries immediately before a benchmark run. The database is authoritative;
indexed-document counts alone do not prove that all fields needed by an answer were ingested.

```sql
-- A01
SELECT COUNT(*) AS patients FROM patients;

-- A02
SELECT patient_no, first_name, last_name
FROM patients
WHERE gender = 'female'
ORDER BY last_name, first_name, patient_no;

-- A03
SELECT patient_no, first_name, last_name
FROM patients
WHERE first_name ILIKE 'P%'
ORDER BY first_name, last_name, patient_no;

-- A04
SELECT gender, COUNT(*) AS patient_count
FROM patients
GROUP BY gender
ORDER BY gender;

-- A05
SELECT DATE_TRUNC('month', encounter_date) AS encounter_month,
       COUNT(*) AS encounter_count
FROM encounters
GROUP BY encounter_month
ORDER BY encounter_count DESC, encounter_month
LIMIT 1;

-- A06
SELECT diagnosis_desc, COUNT(*) AS diagnosis_count
FROM diagnoses
GROUP BY diagnosis_desc
ORDER BY diagnosis_count DESC, diagnosis_desc
LIMIT 1;

-- A07
SELECT ROUND(AVG(length_of_stay_days), 2) AS average_length_of_stay_days,
       COUNT(length_of_stay_days) AS discharged_admissions
FROM admissions;

-- A08: do not use LIMIT 1 because the expected result is tied.
WITH medication_counts AS (
    SELECT m.generic_name, COUNT(*) AS prescribed_items
    FROM prescription_items AS i
    JOIN medication_catalog AS m ON m.id = i.medication_id
    GROUP BY m.generic_name
)
SELECT generic_name, prescribed_items
FROM medication_counts
WHERE prescribed_items = (SELECT MAX(prescribed_items) FROM medication_counts)
ORDER BY generic_name;

-- A09
SELECT is_abnormal, COUNT(*) AS result_count
FROM lab_results
GROUP BY is_abnormal
ORDER BY is_abnormal DESC;

-- A10
SELECT p.patient_no, p.first_name, p.last_name,
       COUNT(DISTINCT e.id) AS encounters,
       COUNT(DISTINCT d.id) AS diagnoses
FROM patients AS p
LEFT JOIN encounters AS e ON e.patient_id = p.id
LEFT JOIN diagnoses AS d ON d.patient_id = p.id
WHERE p.patient_no = 'SYN-2024-0001'
GROUP BY p.patient_no, p.first_name, p.last_name;
```

## Run procedure

For each case:

1. Re-run its PostgreSQL oracle and store the timestamped expected result.
2. Open AI Agents > Auto and select **New Chat**, except for the second prompt in **A10**.
3. Start a monotonic timer immediately before submitting the question exactly as written in the
  matrix.
4. Wait up to 180 seconds for completion.
5. Stop the timer when the final answer is rendered. Capture end-to-end milliseconds, optional TTFO,
  the final answer, selected route/intent when available, browser console errors, and relevant
  server log lines.
6. Compare normalized scalar values or result sets. Ignore ordering unless the question requests it.
7. Assign PASS, PARTIAL, FAIL, or BLOCKED and explain every mismatch.
8. Reopen the conversation and confirm that the final answer persisted unchanged.

Use this run-log template:

| Field | Value |
| --- | --- |
| Date/time | |
| Git revision | |
| Server revision | |
| Model / execution provider | |
| Case ID | |
| Expected | |
| Actual | |
| Status | |
| Precision / recall / F1 | N/A for scalar cases |
| End-to-end latency (ms / s) | |
| Time to first output (ms / s) | Optional |
| Timed out | No / Yes; include timeout duration |
| Route / intent | |
| Browser errors | |
| Server errors | |
| Notes | |

## Run history

### 2026-09-01/02 — complete UI run (current)

| Case | Status | Server total_ms | Actual answer / evidence | Route notes |
| --- | --- | ---: | --- | --- |
| A01 | PASS | 511 | `patient count: 60` | Deterministic SQL; router tier 1 |
| A02 | PASS | 356 | 35 exact female-patient rows | Deterministic SQL |
| A03 | PASS | 4,550 | 9 exact prefix rows | Deterministic SQL; tier-2 route stage 4,011 ms |
| A04 | PASS | 254 | Female 35; male 25 | Deterministic SQL |
| A05 | PASS | 264 | August 2024; 10 encounters | Deterministic SQL |
| A06 | PASS | 2,848 | Infectious gastroenteritis and colitis; 12 | Deterministic SQL; tier-2 route stage 2,475 ms |
| A07 | PASS | 2,836 | 82.44 days over 34 discharged admissions | Deterministic SQL; tier-2 route stage 2,531 ms |
| A08 | PASS | 2,421 | Artemether-Lumefantrine 12 and Ibuprofen 12 | Deterministic tie-preserving CTE; tier-2 route stage 2,215 ms |
| A09 | PASS | 192 | `abnormal result count: 16` | Deterministic SQL; router tier 1 |
| A10 prompt | PASS | 4,618 | Jane Chebet; 2 encounters and 2 diagnoses | Deterministic SQL; first run was also correct at 3,288 ms |
| A10 follow-up | PASS | 299 | Jane Chebet from the live database | Anaphora resolved to `SYN-2024-0001`; deterministic SQL |

List quality for **A02** and **A03** was precision 1.00, recall 1.00, and F1 1.00. There were no
timeouts. The server build was dated 2026-09-01, and every case was submitted through the app UI
with the Auto agent.

Before the fix, the first A10 follow-up attempt was **FAIL** at 73,056 ms. It routed semantically,
spent 44,627 ms in `rewrite_expand` after a model LRU unload and 17,682 ms reranking, then fused a
different record's surname into "Jane Wairimu." This result is retained as regression evidence but
is superseded by the 299 ms passing re-run after deterministic identifier grounding was added.

### Earlier 2026-09-01 partial run (superseded)

The earlier run executed only A01 and A02, recorded A01 without a reliable duration, and timed out
A02 at 180 seconds. Its 50% observed accuracy and 20% coverage are superseded by the complete run
above and must not be used as the current benchmark result.

## Diagnosing failures

| Symptom | Likely layer | Evidence to collect | Corrective action |
| --- | --- | --- | --- |
| Structured question uses semantic retrieval | Router | `/route` result, tier, intent, history flag | Add/adjust deterministic structured patterns and a router fixture; keep structured questions on the SQL/aggregation path |
| Correct table but wrong field/filter | Schema linking | Linked tables/columns, generated SQL, catalog metadata | Improve field aliases and schema descriptions; add the failed wording to NL-to-SQL fixtures |
| Correct values but missing rows | SQL generation or answer rendering | Executed SQL row count versus rendered row count | Remove unintended `LIMIT`, preserve complete tool output, and make truncation explicit with a downloadable/full-results path |
| Extra rows in a filtered list | SQL predicate | Generated SQL and bound literals | Use case-insensitive prefix matching only on the requested field; validate literal and column selection |
| One winner returned when data is tied | SQL ranking | Generated `ORDER BY`/`LIMIT`, oracle tie query | Rank with `DENSE_RANK` or filter by `MAX(count)`; instruct narration to preserve ties |
| Average uses all admissions incorrectly | Aggregate semantics | SQL denominator and null handling | Make "discharged" map to non-null completed stays or the explicit discharge predicate; show denominator in the answer |
| Patient answer mixes records | Join/cardinality | Patient identifier predicate, join keys, pre-aggregation counts | Bind exact patient number and aggregate each one-to-many relation before joining |
| Follow-up loses the patient/result | Conversation memory | Conversation ID, compacted history, rewritten query | Include the prior user question and structured result in rewrite context; add a multi-turn regression fixture |
| Unsupported or fabricated facts | Grounding/generation | Retrieved/tool rows, final prompt, citations | Constrain narration to tool output, reject unsupported claims, and strengthen the grounded-answer verifier |
| Request times out or context overflows | Prompt/model runtime | Input token count, configured budget, stage timings | Rebuild/restart with current prompt budgeting, reduce schema context, and retain the 180-second E2E timeout only as a test ceiling |
| Tool grammar is rejected then falls back | Foundry tool interface | Foundry grammar error and fallback log | Simplify the tool schema to the model-supported JSON subset and retain output validation |
| Browser-only Tauri callback errors | Test harness | `transformCallback` console stack | Run in the Tauri shell or install the IPC/event shim before app startup; do not classify this as an Auto answer error |
| Nested `<button>` console error | Chat conversation list UI | React `validateDOMNesting` stack | Replace the nested action buttons with sibling controls while retaining keyboard and screen-reader behavior |

## Known issues after the 2026-09-01/02 run

- Auto renders the same Chat component as the former general AI Chat feature.
- Router tier-2 model classification adds approximately 2.2–4.1 seconds when tier-1 lexical
  markers cannot decide the route.
- Foundry Local `qwen3-8b` still rejects the tool grammar on the model-planner fallback path. This
  benchmark did not exercise that path because all final answers used deterministic SQL.
- `ConversationList` still emits a React hydration/DOM-nesting warning for a nested `<button>`.

## Recommended correction order

1. Reduce tier-2 router latency without weakening structured-route accuracy.
2. Fix nested conversation controls so console failures indicate pipeline problems rather than UI
   markup defects.
3. Add structured exact-result cases to the automated eval runner; keep this UI benchmark as the
   final end-to-end gate.
4. Add model/tool compatibility coverage for the aggregation schema and remove fallback noise.
5. Retain A10's multi-turn deterministic grounding as a regression test in future full UI runs.

## Relationship to automated evaluation

`eval/run.mjs` remains the fast deterministic gate for router accuracy and retrieval hit-rate/MRR.
This benchmark covers the missing final-answer surface: route selection, SQL planning, execution,
narration, rendering, persistence, and conversational follow-up. A passing router/retrieval run does
not replace this benchmark, and a passing Auto answer does not replace the lower-level suites.
