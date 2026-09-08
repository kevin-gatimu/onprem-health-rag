# Agent test questions

Every question used to exercise the Hospital Agents screen, grouped by the agent that owns it.
One file: the questions, the data behind each one, and which of them a script already checks.

Consolidated from three places that had drifted apart:

- `src/ontology/service_line.rs` → `ServiceLine::examples()` — the live source. Each question is
  paired with the entity concepts it needs, and `GET /agents` hides questions whose concepts are
  not bound in the connected schema. **Change questions here first**; this file follows it.
- `plans/docs/hospital-agents-and-data-map.md` §5 — the same roster in prose.
- `eval/data/router.jsonl` — the machine-readable fixtures, 33 rows, 19 tagged with
  `service_line_expected`. The `Fixture` column below names the row.
- `eval/data/patients.json` — the 200 seeded patients, and the authority for any patient
  identifier that appears in a question.

## How to run

```bash
# Automated: routing (33 cases) and retrieval (7 cases). Needs the server on :8000.
node eval/run.mjs --suite router
node eval/run.mjs --suite retrieval

# Manual: one question against one agent.
TOKEN=$(curl -s -X POST http://localhost:8000/auth/login -H 'content-type: application/json' \
  -d '{"username":"admin","password":"password"}' | jq -r .token)
curl -sS -N -X POST http://localhost:8000/agents/pharmacy \
  -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"question":"Which medicines are out of stock?"}'
```

The SSE reply carries `routed` (route, intent, backend, tier), then `spec` + `rows` for a
structured answer or `citations` for a semantic one, then `token` events. When an answer looks
wrong, read `spec` first — it shows which collection the planner chose, which is where most bad
answers start.

## Data behind these questions

Snapshot taken **2026-09-07** from DocumentDB `onprem_rag.records`: **38,329 records across 63
tables**, ingested from `rag-dev-postgres` → `health_records`.

Sizes worth knowing when you sanity-check an answer:

|                           |                          |                                     |                         |
| ------------------------- | ------------------------ | ----------------------------------- | ----------------------- |
| `patients` 200          | `encounters` 744       | `admissions` 298                  | `beds` 168            |
| `diagnoses` 1,185       | `clinical_notes` 2,122 | `vital_signs` 3,302               | `lab_results` 3,135   |
| `medication_catalog` 51 | `prescriptions` 674    | `medication_administration` 5,681 | `stock_batches` 103   |
| `deliveries` 29         | `surgeries` 91         | `insurance_claims` 240            | `incident_reports` 90 |

### Reference patients

Identifiers are **`PT-00001` … `PT-00200`**, from `patients.json`. An earlier `SYN-P0001` scheme
is gone; a question still carrying it will retrieve nothing, which reads as a broken agent rather
than a stale fixture.

The three used in questions below were chosen because they hold the data their question asks for:

| Patient | Name | Why this one |
| --- | --- | --- |
| `PT-00006` | Esther Omondi (b. 2012-05-28) | has `clinical_notes` — for "review recent notes" and "find patient" |
| `PT-00002` | Dennis Barasa (b. 1969-04-21) | has `patient_medical_history` as well as notes — for "patient history" |
| `PT-00042` | Samuel Adhiambo (b. 1983-02-13) | the Patient Chart example in the agents design doc |

Pick a different one with:

```bash
python -c "import json;d=json.load(open('eval/data/patients.json'));print([p['patient_no'] for p in d][:20])"
```

Regenerate the whole table list before trusting any count:

```bash
docker exec rag-documentdb mongosh "mongodb://docadmin:ChangeMe_Doc123@localhost:10260/onprem_rag?tls=true&tlsAllowInvalidCertificates=true&retrywrites=false" \
  --quiet --eval 'db.records.aggregate([{$group:{_id:"$table",n:{$sum:1}}},{$sort:{_id:1}}]).forEach(d=>print(d._id+"="+d.n));'
```

**Counts in a test file rot.** `eval/data/retrieval.jsonl` asserted that Amlodipine was
`medication_catalog:2` when row 2 is Ibuprofen — six of its seven fixtures pointed at the wrong
records, and the suite reported a 28.6% hit rate for a retriever that was actually scoring 7/7 at
rank 1. Re-verify against the store before believing a red result.

## Reading the tables

`Data behind it` lists the tables bound to the concepts the question needs, with today's row
counts — so "no rows" is distinguishable from "the agent could not answer". `Fixture` names the
`eval/data/router.jsonl` row that checks it automatically; `—` means it is manual only.

---

## Ask — the router

`/agents/ask` has no fixed scope: the router picks the department. These cases pin routing
itself, and are exactly what `node eval/run.mjs --suite router` executes.

| #   | Question                                           | Expected route / intent  | Fixture                     |
| --- | -------------------------------------------------- | ------------------------ | --------------------------- |
| A1  | Hello there                                        | conversational / —      | `greeting`                |
| A2  | Thank you!                                         | conversational / —      | `thanks`                  |
| A3  | What can you do?                                   | capability / —          | `identity`                |
| A4  | How many patients have diabetes?                   | structured / aggregation | `count`                   |
| A5  | Show the trend of malaria cases per month          | structured / trend       | `trend`                   |
| A6  | Review recent clinical notes for patient PT-00006 | semantic / —            | `tier2-semantic`          |
| A7  | List all patients                                  | structured / enumeration | `enumeration`             |
| A8  | Show the distribution of patients by blood type    | structured / aggregation | `distribution`            |
| A9  | Show the top diagnoses by count                    | structured / aggregation | `ranking`                 |
| A10 | Find patient PT-00006                             | semantic / —            | `lookup`                  |
| A11 | Explain what hypertension means                    | semantic / —            | `narrative`               |
| A12 | Show the patient history for PT-00002             | semantic / —            | `patient-history`         |
| A13 | Hey how many patients have diabetes?               | structured / aggregation | `clinical-greeting-guard` |
| A14 | Tell me about PT-00042                             | semantic / —            | `patient-lookup-identity` |

## Patient Chart

`/agents/patient_chart`

| #   | Question                                           | Data behind it                               | Fixture |
| --- | -------------------------------------------------- | -------------------------------------------- | ------- |
| PC1 | What diagnoses does this patient have?             | `diagnoses` 1185, `patients` 200         | —      |
| PC2 | What is this patient allergic to?                  | `patient_allergies` 144, `patients` 200  | —      |
| PC3 | Show me the latest vital signs for this patient.   | `vital_signs` 3302, `patients` 200       | —      |
| PC4 | What medications has this patient been prescribed? | `prescriptions` 674, `patients` 200      | —      |
| PC5 | Who has accessed this patient's record?            | `record_access_log` 1836, `patients` 200 | —      |

## Front Desk

`/agents/front_desk`

| #   | Question                                     | Data behind it                           | Fixture                |
| --- | -------------------------------------------- | ---------------------------------------- | ---------------------- |
| FD1 | Who is booked with Dr Otieno on Thursday?    | `appointments` 724, `providers` 70   | —                     |
| FD2 | Which referrals are still pending?           | `referrals` 108                        | —                     |
| FD3 | What is our no-show rate this month?         | `appointments` 724                     | —                     |
| FD4 | Which clinics run on Tuesday afternoon?      | `provider_schedules` 111               | —                     |
| FD5 | How long do patients wait before being seen? | `appointments` 724, `encounters` 744 | —                     |
| FD6 | What is our no-show rate?                    | —                                       | `front-desk-no-show` |

## Ward Board

`/agents/ward_board`

| #   | Question                                        | Data behind it                                               | Fixture               |
| --- | ----------------------------------------------- | ------------------------------------------------------------ | --------------------- |
| WB1 | How many beds are free in ICU?                  | `beds` 168, `bed_assignments` 298                        | `ward-bed-count`    |
| WB2 | Who is admitted right now?                      | `admissions` 298, `patients` 200                         | `ward-admitted-now` |
| WB3 | Which patients have been in over 14 days?       | `admissions` 298                                           | `ward-long-stay`    |
| WB4 | Which doses were missed on the night shift?     | `medication_administration` 5681, `provider_shifts` 4381 | —                    |
| WB5 | Who was in charge of Medical Ward B last night? | `provider_shifts` 4381, `wards` 12                       | —                    |

## Emergency

`/agents/emergency`

| #   | Question                                            | Data behind it                                 | Fixture          |
| --- | --------------------------------------------------- | ---------------------------------------------- | ---------------- |
| ED1 | How many category 1 patients arrived last month?    | `triage_assessments` 102                     | `ed-category1` |
| ED2 | What is our door-to-doctor time?                    | `triage_assessments` 102, `encounters` 744 | —               |
| ED3 | How many arrived by ambulance?                      | `triage_assessments` 102                     | `ed-ambulance` |
| ED4 | How many emergency admissions were there last week? | `admissions` 298, `triage_assessments` 102 | —               |

## Maternity

`/agents/maternity`

| #   | Question                                                     | Data behind it                             | Fixture                  |
| --- | ------------------------------------------------------------ | ------------------------------------------ | ------------------------ |
| MA1 | How many deliveries last month, and how many were caesarean? | `deliveries` 29                          | —                       |
| MA2 | What is our stillbirth rate?                                 | `deliveries` 29, `newborns` 32         | —                       |
| MA3 | Which mothers had a postpartum haemorrhage?                  | `deliveries` 29                          | —                       |
| MA4 | How many babies were low birth weight?                       | `newborns` 32                            | —                       |
| MA5 | Who is due for an antenatal visit?                           | `antenatal_visits` 174, `patients` 200 | —                       |
| MA6 | How many deliveries last month?                              | —                                         | `maternity-deliveries` |
| MA7 | How many caesarean deliveries last month?                    | —                                         | `maternity-caesarean`  |

## Theatre

`/agents/theatre`

| #   | Question                                          | Data behind it                       | Fixture                   |
| --- | ------------------------------------------------- | ------------------------------------ | ------------------------- |
| TH1 | What is on tomorrow's operating list?             | `surgeries` 91                     | —                        |
| TH2 | How many operations were cancelled, and why?      | `surgeries` 91                     | —                        |
| TH3 | What is our average theatre time for a caesarean? | `surgeries` 91, `procedures` 265 | —                        |
| TH4 | Which cases had no consent recorded?              | `consents` 279, `surgeries` 91   | —                        |
| TH5 | How many operations were cancelled?               | —                                   | `theatre-cancelled`     |
| TH6 | What is on tomorrow's list?                       | —                                   | `theatre-list-tomorrow` |

## Pharmacy

`/agents/pharmacy`

| #   | Question                                           | Data behind it                                   | Fixture                |
| --- | -------------------------------------------------- | ------------------------------------------------ | ---------------------- |
| PH1 | Which medicines are out of stock?                  | `stock_batches` 103, `stock_movements` 1143  | `pharmacy-stock`     |
| PH2 | What expires within 30 days?                       | `stock_batches` 103                            | `pharmacy-expiry`    |
| PH3 | Which patients are on an interacting combination?  | `drug_interactions` 12, `prescriptions` 674  | —                     |
| PH4 | How much amoxicillin did we dispense last quarter? | `prescriptions` 674, `medication_catalog` 51 | `pharmacy-dispensed` |

## Diagnostics

`/agents/diagnostics`

| #   | Question                                   | Data behind it                            | Fixture                  |
| --- | ------------------------------------------ | ----------------------------------------- | ------------------------ |
| DX1 | Which results are critical and unreviewed? | `lab_results` 3135                      | `diagnostics-critical` |
| DX2 | What is our lab turnaround time?           | `lab_orders` 1181, `lab_results` 3135 | —                       |
| DX3 | How many units of O-negative do we have?   | `blood_units` 180                       | —                       |
| DX4 | Were there any transfusion reactions?      | `transfusions` 57                       | —                       |

## Revenue

`/agents/revenue`

| #   | Question                                | Data behind it                                                                   | Fixture                     |
| --- | --------------------------------------- | -------------------------------------------------------------------------------- | --------------------------- |
| RV1 | What is outstanding by insurer?         | `billing_encounters` 744, `insurance_claims` 240, `insurance_providers` 10 | `revenue-outstanding`     |
| RV2 | Which claims were rejected, and why?    | `insurance_claims` 240                                                         | —                          |
| RV3 | What did we collect last month?         | `payments` 859                                                                 | —                          |
| RV4 | What is our average bill by department? | `billing_encounters` 744, `departments` 18                                   | —                          |
| RV5 | Which claims were rejected?             | —                                                                               | `revenue-rejected-claims` |

## Quality & Safety

`/agents/quality_safety`

| #   | Question                               | Data behind it                    | Fixture           |
| --- | -------------------------------------- | --------------------------------- | ----------------- |
| QS1 | How many falls this quarter?           | `incident_reports` 90           | `quality-falls` |
| QS2 | What is our inpatient mortality rate?  | not bound                         | —                |
| QS3 | Which TB cases have not been notified? | `notifiable_disease_reports` 85 | —                |
| QS4 | Show me break-glass record access.     | `record_access_log` 1836        | —                |

## Workforce

`/agents/workforce`

| #   | Question                                       | Data behind it                             | Fixture                      |
| --- | ---------------------------------------------- | ------------------------------------------ | ---------------------------- |
| WF1 | Whose practising licence expires this quarter? | `provider_licenses` 66                   | `workforce-licence-expiry` |
| WF2 | Who is on call tonight?                        | `provider_shifts` 4381, `providers` 70 | —                           |
| WF3 | Which doctor saw the most patients last month? | `encounters` 744, `providers` 70       | —                           |
| WF4 | How much leave is booked for December?         | `provider_time_off` 95                   | —                           |

## Chronic Care

`/agents/chronic_care`

| #   | Question                                          | Data behind it                                  | Fixture                   |
| --- | ------------------------------------------------- | ----------------------------------------------- | ------------------------- |
| CC1 | Who has been lost to follow-up in the HIV clinic? | `program_enrollments` 81, `care_programs` 7 | `chronic-lost-followup` |
| CC2 | Who is due for review this week?                  | `program_enrollments` 81                      | —                        |
| CC3 | How many patients are on TB treatment?            | `program_enrollments` 81, `care_programs` 7 | —                        |
| CC4 | Which vaccines are due this month?                | `vaccine_catalog` 12, `immunizations` 372   | —                        |

## Facilities

`/agents/facilities`

| #   | Question                                          | Data behind it                                  | Fixture |
| --- | ------------------------------------------------- | ----------------------------------------------- | ------- |
| FA1 | Which equipment is out of service?                | `equipment` 70                                | —      |
| FA2 | What is overdue for servicing?                    | `equipment_maintenance` 191                   | —      |
| FA3 | How much downtime did we have on the ventilators? | `equipment` 70, `equipment_maintenance` 191 | —      |

---

## Known failures (verified 2026-09-07)

Run these first: they are the ones that fail today, with the cause already traced.

| Question                                           | Agent            | What happens                                                | Cause                                                                                                                                                                                                                         |
| -------------------------------------------------- | ---------------- | ----------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| What can you do?                                   | Ask              | routes`conversational`, should be `capability`          | router fixture`identity`                                                                                                                                                                                                    |
| Review recent clinical notes for patient PT-00006 | Ask              | routes`structured/aggregation`, should be `semantic`    | router fixture`tier2-semantic`                                                                                                                                                                                              |
| What is on tomorrow's operating list?              | Theatre          | routes`structured/aggregation`, should be `enumeration` | router fixture`theatre-list-tomorrow`                                                                                                                                                                                       |
| list the medicines we have?                        | Ask, Pharmacy    | answers with a**count**, not a list                   | `agents/routes.rs` sends every structured intent to the aggregation planner; the `QueryIntent::Enumeration` branch in `answer/mod.rs` that plans a `run_list_records` spec is never reached                           |
| How many patients have diabetes?                   | Ask              | answers**8**                                          | counts rows in`patient_medical_history`; the same condition in `diagnoses` covers **25 distinct patients**. Nothing reconciles the two, and the metric is a row count where the question asks for distinct patients |
| What is our inpatient mortality rate?              | Quality & Safety | cannot be answered                                          | the`Mortality` concept has no table in this seed — the only unbound question in this file                                                                                                                                  |

Router accuracy on the automated suite is **30/33 (90.9 %)** against a 0.95 gate. Retrieval is
**7/7, MRR 1.000**.

## Keeping this file honest

- Add a question to `ServiceLine::examples()` first, then mirror it here. The code list is what the
  UI shows and what `GET /agents` filters; a question that exists only in Markdown is untested.
- Give anything worth guarding a row in `eval/data/router.jsonl` with `service_line_expected`, so
  it runs in CI rather than depending on someone remembering to type it.
- Re-verify row counts after any re-ingest, using the command above.

## Not folded in

`eval/TAB_STRESS_QUESTIONS.md` (368 prompts) and `eval/QUESTIONS.md` (A/S series) are still
organised by the **previous** tab taxonomy — Auto, Health Query, Trends, Patient Lookup,
Summarize — which no longer exists in the app. Their ground truth is also pinned to an older,
smaller seed ("1,603 chunks across 25 tables", 2026-09-04) against today's 38,329 across 63.
They are left in place because their answer key is detailed and worth mining, but they should be
remapped onto this roster or retired rather than trusted as-is.
