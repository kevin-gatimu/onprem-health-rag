# Test Question Reference

All questions used to exercise the AI Agents screen (Auto + dedicated tabs), with
ground truth from `rag-dev-postgres` (`health_records`) and latest observed status.
Companion to [AUTO_BENCHMARK.md](AUTO_BENCHMARK.md), which holds scoring rules and full run logs,
and to [TAB_STRESS_QUESTIONS.md](TAB_STRESS_QUESTIONS.md) — a much larger per-tab sweep with a full
ground-truth answer key, written to find break points rather than to guard known-good behavior.

Ground truth queries run via:
`docker exec rag-dev-postgres psql -U health -d health_records -c "..."`

## A-series — structured questions (Auto tab, deterministic SQL path)

| # | Question | Ground truth | Status (2026-09-01 run) |
|---|----------|--------------|--------------------------|
| A01 | How many patient records are indexed? | 60 patients | PASS · 511 ms |
| A02 | List the names and patient numbers of all female patients. | 35 rows | PASS · 356 ms |
| A03 | Show me patients whose first name starts with P | 9 rows | PASS · 4,550 ms (tier-2 route 4.0 s) |
| A04 | How many female and male patients are there? | female 35 / male 25 | PASS · 254 ms |
| A05 | Which month had the most encounters, and how many? | August 2024, 10 | PASS · 264 ms |
| A06 | What is the most common diagnosis, and how many patients have it? | Infectious gastroenteritis and colitis, 12 | PASS · 2,848 ms |
| A07 | What is the average length of stay for discharged admissions? | 82.44 days over 34 admissions | PASS · 2,836 ms |
| A08 | Which medications were prescribed most often? | Tie: Artemether-Lumefantrine 12, Ibuprofen 12 | PASS · 2,421 ms |
| A09 | How many lab results were abnormal? | 16 | PASS · 192 ms |
| A10a | Show encounter and diagnosis counts for patient SYN-2024-0001. | Jane Chebet, 2 encounters / 2 diagnoses | PASS · 3,288–4,618 ms |
| A10b | *(follow-up, same conversation)* What is the patient's name? | Jane Chebet | PASS · 299 ms (anaphoric follow-up fix; previously 73 s + wrong name) |

## S-series — semantic and structured/SQL expansion questions (Auto tab)

| # | Question | Ground truth | Status |
|---|----------|--------------|--------|
| S01 | What conditions has Jane Chebet been treated for? | Atherosclerotic heart disease (I25.1, primary), Asthma unspecified (J45.9, secondary) | **FAIL** · 67.4 s, gated ("I don't have relevant records"); rerank_top_score 0.0297 |
| S02 | Which patients have been diagnosed with asthma? | patients with J45.9 diagnoses | planned |
| S03 | Summarize the medical history of patient SYN-2024-0001. | 2 encounters, dx I25.1 + J45.9 | planned |
| S04 | Have encounter volumes been going up or down over time? | monthly encounter counts, peak Aug 2024 | planned |
| S05 | Tell me about Jane Chebet. | patient card SYN-2024-0001, F, blood type etc. | planned |
| S06 | Which providers ordered the most lab panels? | Top 10 providers by `lab_orders` count via `lab_orders.ordered_by → providers.id`; deterministic SQL (Stella Chebet 9, then 7/7/7) | PASS · 2026-09-02, matches psql ground truth; execute ~470 ms (21.7 s total on cold router warm-up); EXPLAIN cost 17.81. Previously 175 s → refusal via LLM planner |
| S07 | Which providers prescribed the most medications? | Top 10 providers by `prescriptions` count via `prescriptions.prescriber_id → providers.id`; deterministic SQL (Stella Chebet 9) | PASS · 2026-09-02, 3,976 ms (route 3.7 s tier-2); EXPLAIN cost 18.15 |
| S08 | Which providers had the most encounters? | Top 10 providers by `encounters` count via `encounters.provider_id → providers.id`; deterministic SQL | planned |
| S09 | Which counties had the most admissions? | Unsupported dimension; deterministic matcher must return `None` and fall through to the local planner | planned |

The S06–S08 grouped-count join class is deterministic and does not call the local model planner.
In particular, "Which providers ordered the most lab panels?" resolves the join column from schema
card FK metadata rather than assuming `provider_id`.

### S01 findings (root causes identified, fixes pending)

1. **Chunking gap** — child-table chunks (`diagnoses`, `encounters`, …) carry only UUID
   foreign keys (`patient_id`, `encounter_id`) and no patient name/number, so name-based
   semantic questions can never retrieve them. Verified: `diagnoses` chunk for I25.1 has
   `patient_id: b572718d-…` and no name. Fix direction: denormalize human identifiers
   (patient_no, first/last name) into child chunks at ingest, or entity-resolve names →
   patient_no pre-retrieval and filter.
2. **Latency** — 67,434 ms total: `rewrite_expand` 44,416 ms (Foundry LRU unloaded
   qwen3-8b mid-pipeline, reload cost), `rerank` 18,630 ms, `route` 2,389 ms.
3. **Gate worked as designed** — refusing on 0.0297 top score was correct behavior; the
   failure is upstream (retrieval), not the gate.

## T-series — dedicated tab sweep (planned)

Same intents as the Auto sweep but sent from each dedicated tab, to verify per-tab
kind routing and output shape.

| # | Tab | Question |
|---|-----|----------|
| T01 | Health Queries | How many lab results were abnormal? |
| T02 | Health Queries | Which patients have asthma? |
| T03 | Trends | Which month had the most encounters, and how many? |
| T04 | Trends | How have prescriptions trended by month? |
| T05 | Patient Lookup | Look up patient SYN-2024-0001. |
| T06 | Patient Lookup | Find Jane Chebet's record. |
| T07 | Summarize | Summarize the medical history of patient SYN-2024-0001. |
| T08 | Summarize | Give me an overview of the most recent encounters. |
| T09 | Health Queries | Which providers ordered the most lab panels? |
| T10 | Trends | Which doctors had the most encounters? |

## Known issues affecting these runs

- Tier-2 router model classification adds 2.2–4.1 s to `route` stage.
- Foundry LRU unload evicts the GPU model between requests → 40 s+ reload on the next
  `rewrite_expand`.
- Rerank of 30 candidates costs ~18 s on this hardware.
- Nested `<button>` React hydration warning in ConversationList (cosmetic).
