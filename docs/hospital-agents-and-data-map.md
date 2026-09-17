<!-- markdownlint-disable MD013 -->

# Hospital agents and their data map

How the AI Agents surface should be divided, what each agent is called, and which data each agent
owns. Companion to
[Routing, agents, and structured query](routing-agents-and-structured-query.md), which covers the
routing machinery; this document covers the **product shape** and the **data boundary**.

Status: **Planned — implementation plans in [plans/new/](../new/00-README.md)
([01 ontology and schema binding](../new/01-service-line-ontology-and-schema-binding.md),
[05 hospital agents server](../new/05-hospital-agents-server.md)).** The current build ships `auto`,
`health_query`, `trends`, `patient_lookup`, `summarize` and `chat`. Nothing below is implemented yet.

The roster is a fixed **hospital ontology**: 13 service lines plus Ask, each owning *entity concepts*
(Admission, Delivery, Bill…), not physical tables. Which tables a deployment's schema provides for
each concept is decided **per source at catalog time** by a `SchemaBinding` (§8); the 64-table
dev-seed mapping in §4 is one binding, not the design.

---

## 1. The organising principle: agents follow the hospital, not the query

The current tabs are named after query mechanics — *Health Query* (counting), *Trends* (bucketing),
*Summarize* (narrating). That taxonomy is invisible to the people who use a hospital. A midwife does
not think "this is an aggregation question"; she thinks "how many deliveries did we have last night".

So each agent should map to a **service line** — a department or function that a real hospital
staffs, budgets and reports on separately — and own the tables that service line writes to. One
agent owning ten related tables is correct and desirable: `deliveries`, `newborns` and
`antenatal_visits` are meaningless apart, and a maternity agent that can join them answers questions
neither a "counting agent" nor a "narrating agent" can.

This has three practical consequences:

1. **Smaller schema-linking search space.** Text-to-SQL picks tables from an allow-list of 6–11
   instead of 64, which is the single biggest lever on structured-query accuracy.
2. **A persona with real vocabulary.** The maternity agent knows that "para 2" and "SVD" and
   "Apgar" are column values, not noise — and learns them from the bound schema's enum values, not
   from a hardcoded list.
3. **Answerable scope questions.** "What can you do?" gets a truthful, bounded answer per tab.

Concepts are shared where the hospital shares them. Patient and Encounter belong to everyone;
Admission belongs to Ward Board, Emergency, Maternity and Theatre. Ownership here means *"this agent
may read it and is prompted about it"*, not exclusivity.

---

## 2. The roster

Names chosen for what a hospital calls the function, not what the query engine does. The
"Primary tables" column shows the **dev-seed binding**; on another hospital's schema the same agent
owns whatever tables bind to its concepts (§4, §8).

| # | Agent | Replaces | Owns the question | Primary tables (dev seed) |
|---|---|---|---|---|
| 1 | **Ask** | `auto` | Routes anything to the right agent below | — |
| 2 | **Patient Chart** | `patient_lookup` | Everything about *one* patient | `patients`, `encounters`, `diagnoses`, `clinical_notes`, `vital_signs`, `patient_allergies`, `patient_medical_history`, `patient_family_history`, `immunizations`, `patient_documents`, `counties` |
| 3 | **Front Desk** | — *(new)* | Bookings, clinics, referrals, cover | `appointments`, `provider_schedules`, `provider_time_off`, `referrals`, `patient_insurance`, `insurance_providers` |
| 4 | **Ward Board** | — *(new)* | Who is in, where, and for how long | `admissions`, `bed_assignments`, `beds`, `wards`, `patient_transfers`, `provider_shifts`, `medication_administration` |
| 5 | **Emergency** | — *(new)* | The front door: triage, waits, acuity | `triage_assessments`, `encounters`, `vital_signs`, `admissions`, `referrals` |
| 6 | **Maternity** | — *(new)* | Antenatal through delivery to the newborn | `antenatal_visits`, `deliveries`, `newborns`, `admissions`, `encounters`, `program_enrollments` |
| 7 | **Theatre** | — *(new)* | The operating list and its utilisation | `surgeries`, `procedures`, `procedure_codes`, `consents`, `equipment` |
| 8 | **Pharmacy** | — *(new)* | Prescribing, dispensing, stock, safety | `prescriptions`, `prescription_items`, `medication_catalog`, `medication_administration`, `stock_batches`, `stock_movements`, `drug_interactions`, `allergen_catalog`, `cds_alerts` |
| 9 | **Diagnostics** | — *(new)* | Lab, imaging and blood bank | `lab_orders`, `lab_results`, `lab_test_catalog`, `imaging_orders`, `blood_units`, `transfusions` |
| 10 | **Revenue** | — *(new)* | Bills, claims, collections | `billing_encounters`, `billing_items`, `insurance_claims`, `payments`, `patient_insurance`, `insurance_providers` |
| 11 | **Quality & Safety** | — *(new)* | Incidents, mortality, notifications, privacy | `incident_reports`, `mortality_records`, `patient_feedback`, `notifiable_disease_reports`, `record_access_log`, `cds_alerts`, `icd10_codes` |
| 12 | **Workforce** | — *(new)* | Staffing, licences, rota, workload | `providers`, `provider_licenses`, `provider_schedules`, `provider_time_off`, `provider_shifts`, `departments` |
| 13 | **Chronic Care** | — *(new)* | Longitudinal clinic registers | `care_programs`, `program_enrollments`, `vaccine_catalog`, `immunizations` |
| 14 | **Facilities** | — *(new)* | Equipment, downtime, bed estate | `equipment`, `equipment_maintenance`, `wards`, `beds` |
| — | **Service Trends** | `trends` | *Cross-cutting.* Volumes over time for whichever service the question names | Delegates to the owning agent's tables |
| — | **Handover** | `summarize` | *Cross-cutting.* Narrative synthesis for a shift, a patient or a discharge | Delegates to the owning agent's tables |

**Service Trends** and **Handover** stay as *modes*, not departments — "how has X trended" and
"summarise X" apply to every service line. They are an `AgentMode { Ask, Trends, Handover }` flag on
the department agent rather than separate tabs, so *"how have deliveries trended?"* reaches
Maternity's tables with a time-bucketing instruction rather than a generic trends agent that has to
guess the schema. Trends forces a `Trend` shape (default bucket month, last 12 months); Handover
forces a cohort-scoped Hybrid or a filtered semantic synthesis over the last 24 h with SBAR headings
and citations ([plan 05 §6](../new/05-hospital-agents-server.md)).

### Names considered and rejected

| Rejected | Why |
|---|---|
| *Health Query* | Names the mechanism, not the job. Everything here is a health query. |
| *Cohort Finder* | Accurate but research-flavoured; clinicians say "who is on the ward", not "define a cohort". |
| *Bed Board* | Nearly right, but real boards cover staffing and drug rounds too — *Ward Board* is the wider, truer scope. |
| *Scheduling Desk* | Longer than *Front Desk* and less recognisable to reception staff. |
| *Medicines Safety* | Too narrow: the same agent must answer stock questions, which are not safety questions. |
| *Lab* | Excludes imaging and blood bank, which share the "ordered, resulted, reported" shape. |

---

## 3. Recommended rollout

Fourteen tabs is a menu, not a product. Ship six and let **Ask** reach the rest:

**Tier 1 — the daily jobs (build these):** Ask · Patient Chart · Front Desk · Ward Board · Pharmacy · Diagnostics

**Tier 2 — service lines with a distinct audience:** Maternity · Theatre · Emergency · Revenue

**Tier 3 — periodic, management-facing:** Quality & Safety · Workforce · Chronic Care · Facilities

Tier 2 and 3 agents still work through **Ask** before they get their own tab: the ownership table
below is what makes an unlisted agent reachable. A tab is shown only when its service line is
**usable** on some connected source (§8); the roster comes from `GET /agents`, not a hardcoded list.

---

## 4. Concept ownership (fixed)

Service lines own **entity concepts**. The table is a constant in `ontology/service_line.rs`
([plan 01 §4](../new/01-service-line-ontology-and-schema-binding.md)) and never changes per
deployment. Multiple owners per concept are intentional (§1). A table acquires its service lines
through the concept it binds to.

| Service line | Owned concepts |
|---|---|
| Patient Chart | Diagnosis, ClinicalNote, VitalSign, Allergy, MedicalHistory, FamilyHistory, Immunization, Document, Geography |
| Front Desk | Appointment, ProviderSchedule, ProviderTimeOff, Referral, InsurancePolicy, Insurer |
| Ward Board | Admission, BedAssignment, Bed, Ward, Transfer, Shift, MedicationAdministration, VitalSign |
| Emergency | Triage, Encounter, VitalSign, Admission, Referral |
| Maternity | AntenatalVisit, Delivery, Newborn, Admission, ProgramEnrollment |
| Theatre | Surgery, Procedure, ProcedureCode, Consent, Equipment, Admission |
| Pharmacy | Prescription, PrescriptionItem, Medication, MedicationAdministration, StockBatch, StockMovement, DrugInteraction, Allergen, CdsAlert, Allergy |
| Diagnostics | LabOrder, LabResult, LabTest, ImagingOrder, BloodUnit, Transfusion |
| Revenue | Bill, BillItem, Claim, Payment, InsurancePolicy, Insurer |
| Quality & Safety | Incident, Mortality, Feedback, NotifiableDisease, AccessLog, CdsAlert, DiagnosisCode |
| Workforce | Provider, License, ProviderSchedule, ProviderTimeOff, Shift, Department |
| Chronic Care | CareProgram, ProgramEnrollment, Vaccine, Immunization |
| Facilities | Equipment, EquipmentMaintenance, Ward, Bed |

**Shared concepts** — `Patient, Encounter, Provider, Department, DiagnosisCode` — are readable by
every agent (§6 rule 2). **Patient Chart** additionally treats every table with a `patient_path`
(≤ 2 FK hops to the Patient table) as readable when the question names a patient, so "what is
PT-00042 allergic to / owed / booked for" works from the chart without hopping tabs.

A service line is **usable** for a source when at least one owned concept is bound with confidence
≥ 0.55 (`ONPREM_BINDING_MIN_CONFIDENCE`). Unusable lines are hidden in the UI and never routed to.

### Dev-seed binding example

The binder must reproduce this mapping for `docker/dev-postgres` **without any override**; it is the
regression fixture `ontology/tests/dev_seed_binding.json` (≥ 62/64 exact concept matches, 64/64
service-line superset). All 64 tables, the concept each binds to, and the agents that answer for
each. No orphans — an unowned table is a table no question can reach.

| Table | Concept | Owning agent(s) |
|---|---|---|
| `patients` | Patient | *shared: all* |
| `counties` | Geography | Patient Chart |
| `patient_allergies` | Allergy | Patient Chart, Pharmacy |
| `patient_medical_history` | MedicalHistory | Patient Chart |
| `patient_family_history` | FamilyHistory | Patient Chart |
| `patient_documents` | Document | Patient Chart |
| `immunizations` | Immunization | Patient Chart, Chronic Care |
| `encounters` | Encounter | Patient Chart, Emergency *(shared: all)* |
| `diagnoses` | Diagnosis | Patient Chart |
| `clinical_notes` | ClinicalNote | Patient Chart, Handover mode |
| `vital_signs` | VitalSign | Patient Chart, Ward Board, Emergency |
| `appointments` | Appointment | Front Desk |
| `provider_schedules` | ProviderSchedule | Front Desk, Workforce |
| `provider_time_off` | ProviderTimeOff | Front Desk, Workforce |
| `referrals` | Referral | Front Desk, Emergency |
| `triage_assessments` | Triage | Emergency |
| `admissions` | Admission | Ward Board, Emergency, Maternity, Theatre |
| `bed_assignments` | BedAssignment | Ward Board |
| `beds` | Bed | Ward Board, Facilities |
| `wards` | Ward | Ward Board, Facilities |
| `patient_transfers` | Transfer | Ward Board |
| `provider_shifts` | Shift | Ward Board, Workforce |
| `medication_administration` | MedicationAdministration | Ward Board, Pharmacy |
| `antenatal_visits` | AntenatalVisit | Maternity |
| `deliveries` | Delivery | Maternity |
| `newborns` | Newborn | Maternity |
| `surgeries` | Surgery | Theatre |
| `procedures` | Procedure | Theatre |
| `procedure_codes` | ProcedureCode | Theatre |
| `consents` | Consent | Theatre |
| `prescriptions` | Prescription | Pharmacy |
| `prescription_items` | PrescriptionItem | Pharmacy |
| `medication_catalog` | Medication | Pharmacy |
| `stock_batches` | StockBatch | Pharmacy |
| `stock_movements` | StockMovement | Pharmacy |
| `drug_interactions` | DrugInteraction | Pharmacy |
| `allergen_catalog` | Allergen | Pharmacy |
| `cds_alerts` | CdsAlert | Pharmacy, Quality & Safety |
| `lab_orders` | LabOrder | Diagnostics |
| `lab_results` | LabResult | Diagnostics |
| `lab_test_catalog` | LabTest | Diagnostics |
| `imaging_orders` | ImagingOrder | Diagnostics |
| `blood_units` | BloodUnit | Diagnostics |
| `transfusions` | Transfusion | Diagnostics |
| `billing_encounters` | Bill | Revenue |
| `billing_items` | BillItem | Revenue |
| `insurance_claims` | Claim | Revenue |
| `payments` | Payment | Revenue |
| `patient_insurance` | InsurancePolicy | Revenue, Front Desk |
| `insurance_providers` | Insurer | Revenue, Front Desk |
| `incident_reports` | Incident | Quality & Safety |
| `mortality_records` | Mortality | Quality & Safety |
| `patient_feedback` | Feedback | Quality & Safety |
| `notifiable_disease_reports` | NotifiableDisease | Quality & Safety |
| `record_access_log` | AccessLog | Quality & Safety |
| `icd10_codes` | DiagnosisCode | Quality & Safety *(shared: all)* |
| `providers` | Provider | Workforce *(shared: all)* |
| `provider_licenses` | License | Workforce |
| `departments` | Department | Workforce *(shared: all)* |
| `care_programs` | CareProgram | Chronic Care |
| `program_enrollments` | ProgramEnrollment | Chronic Care, Maternity |
| `vaccine_catalog` | Vaccine | Chronic Care |
| `equipment` | Equipment | Facilities, Theatre |
| `equipment_maintenance` | EquipmentMaintenance | Facilities |

---

## 5. What each agent can now answer

Questions that were impossible before the schema rebuild are marked **new**.

**Patient Chart** — "Tell me about PT-00042." · "What is this patient allergic to?" ·
"What has changed since their last visit?" · "Who has accessed this record?" **new**

**Front Desk** — "Who is booked with Dr Otieno on Thursday?" **new** · "What is our no-show rate?"
**new** · "Which clinics run on Tuesday afternoon?" **new** · "How long do patients wait before
being seen?" **new** · "Which referrals are still pending?" **new**

**Ward Board** — "How many beds are free in ICU?" **new** · "Who is admitted right now?" **new** ·
"Which patients have been in over 14 days?" · "Which doses were missed on the night shift?" **new** ·
"Who was in charge of Medical Ward B last night?" **new**

**Emergency** — "How many category 1 patients arrived last month?" **new** · "What is our
door-to-doctor time?" **new** · "How many arrived by ambulance?" **new**

**Maternity** — "How many deliveries last month, and how many were caesarean?" **new** ·
"What is our stillbirth rate?" **new** · "Which mothers had a postpartum haemorrhage?" **new** ·
"How many babies were low birth weight?" **new** · "Who is due for an antenatal visit?" **new**

**Theatre** — "What is on tomorrow's list?" **new** · "How many operations were cancelled, and why?"
**new** · "What is our average theatre time for a caesarean?" **new** · "Which cases had no consent
recorded?" **new**

**Pharmacy** — "Which medicines are out of stock?" **new** · "What expires within 30 days?" **new** ·
"Which patients are on an interacting combination?" · "How much amoxicillin did we dispense last
quarter?" **new** · "Which doses were refused, and why?" **new**

**Diagnostics** — "Which results are critical and unreviewed?" **new** · "What is our lab
turnaround time?" **new** · "How many units of O-negative do we have?" **new** ·
"Were there any transfusion reactions?" **new**

**Revenue** — "What is outstanding by insurer?" · "Which claims were rejected, and why?" **new** ·
"What did we collect by M-Pesa last month?" · "What is our average bill by department?"

**Quality & Safety** — "How many falls this quarter?" **new** · "What is our inpatient mortality
rate?" **new** · "Which deaths are pending certification?" **new** · "Which TB cases have not been
notified?" **new** · "Show me break-glass record access." **new**

**Workforce** — "Whose practising licence expires this quarter?" **new** · "Who is on call tonight?"
**new** · "Which doctor saw the most patients last month?" · "How much leave is booked for December?"
**new**

**Chronic Care** — "Who has been lost to follow-up in the HIV clinic?" **new** · "Who is due for
review this week?" **new** · "How many patients are on TB treatment?" **new**

**Facilities** — "Which equipment is out of service?" **new** · "What is overdue for servicing?"
**new** · "How much downtime did we have on the ventilators?" **new**

---

## 6. How the mapping is enforced in code

The concept table (§4) is a constant; the *physical* map is the per-source `SchemaBinding`
([plan 01 §3](../new/01-service-line-ontology-and-schema-binding.md)). An agent's scope is
`binding.tables_for(line)` — tables bound to owned concepts plus the shared concepts — and four hooks
read it. All four already exist in the code and take a table set; the change is what feeds them
([plan 05 §3](../new/05-hospital-agents-server.md)).

| Hook | File | Change |
|---|---|---|
| Agent identity | `agents/kind.rs` (`AgentKind { Ask, Line(ServiceLine) }`, `AgentMode`) | Replaces the mechanism-named kinds; `Line(l)` ⇒ scope `binding.tables_for(l)`; `Ask` ⇒ scope from the router decision or none. Model roles move to `foundry/router.rs::ModelRole`. |
| Text-to-SQL schema linking | `nl2sql/linker.rs` | New `scope` argument; candidate `TableCard`s are filtered to scope **before** scoring. An explicit mention outside scope ("payments" on Maternity) yields a redirect to the owning agent, never a join. This is where accuracy is won: linking against 8 tables instead of 64. |
| Aggregation catalog | `aggregation/catalog.rs` | `Catalog::scoped(scope)` for the planner prompt and validation, so the model cannot name a collection the agent does not own. |
| Semantic retrieval | `retrieval/mod.rs` | `RetrievalFilter { tables: scope, explicit: kind != Ask }` applied to the vector and `$text` stages, so Maternity does not retrieve billing chunks ([plan 04 §4](../new/04-structured-execution-and-fallbacks.md)). |

The persona (`agents/persona.rs`) is generated from the same binding: role line, the bound concepts
with the hospital's own table names and enum vocabulary (PII columns never listed), grounding rules,
mode block, focus block. "What can you do?" is answered from the binding without a model.

A "no orphans" test asserts every table in the live `schema_catalog` is in the scope of at least one
service line — as `cargo test` against the dev-seed fixture and live as an admin diagnostic on
`GET /sources/<id>/binding` (`orphans: []`). That test is what stops the map from rotting the next
time a table is added.

Two rules worth stating explicitly:

- **Ownership is a read allow-list, not a security boundary.** Access control stays in the JWT/RBAC
  layer; this map only shapes what an agent looks at.
- **Patient, Encounter, Provider, Department and DiagnosisCode are readable by every agent.**
  Almost every real question joins at least one of them, and excluding them would force agents to
  answer with foreign keys instead of names.

---

## 7. Open questions

- Should **Ask** be able to fan out across two agents for a genuinely cross-service question
  ("did the patients who had surgery last month pay their bills?"), or should it pick one and say
  what it excluded? Picking one is simpler and more honest; fan-out is more useful.
- Do **Service Trends** and **Handover** stay as modes, or become visible toggles on each tab?
- Should tabs be filtered by the signed-in user's role, so a billing clerk does not see Patient Chart?
  The RBAC layer can already express this; the product decision has not been made.

---

## 8. Binding to other hospital schemas

A hospital with `tbl_patient`, `PatientMaster`, `enc_visits`, `IPD_Admission` or `OT_Case` gets the
same agents because binding is computed, not authored
([plan 01 §5](../new/01-service-line-ontology-and-schema-binding.md)). `build_binding(source_id)`
runs after every successful catalog refresh and whenever overrides are saved; the result is stored in
`schema_bindings` (last five versions in `schema_binding_history`) and loaded into `AppState`.

**Binder signals.** Per table, each `EntityConcept` is scored 0..1 as a weighted sum and the argmax
wins:

| Signal | Weight | How |
|---|---|---|
| Name tokens | 0.35 | Normalise the table name (strip schema and prefixes `tbl_`, `t_`, `dim_`, `fact_`, `hms_`; split snake/camel; singularise; stem), Jaccard against the concept's name tokens and synonyms; exact singular/plural match → 1.0 |
| Column tokens | 0.30 | Fraction of the concept's expected column tokens (`admission_date`, `discharge`, `ward`, `los`…) present as substrings of any column name |
| Required roles | 0.15 | Fraction of required `ColumnRole`s satisfied (e.g. Admission needs `PatientRef` + `EventTime`) |
| Embedding | 0.15 | Cosine of the card's BGE-M3 vector against the concept description (descriptor vectors computed once at boot) |
| FK shape | 0.05 | FK to the Patient-bound table favours patient-facing concepts; FK to Medication favours Prescription / StockBatch… |

Two passes: anchors first (Patient, Provider, Encounter, Department, Medication, Ward), then the rest
with FK shape available. Ties (Δ < 0.05) resolve toward the concept whose owning line is not yet
covered. Column roles are assigned in order: override alias → PK/FK metadata (`PatientRef`,
`WardRef`…) → name tokens + type (`EventTime`, `Amount`, `Status`, `Gender`…) → profile
(low-cardinality text → `Category`/`Status`) → `Unknown`. Exactly one `EventTime` per table,
preferring domain names over `created_at`/`updated_at`. Enum values (≤ 25) are captured for
`Status | Category | Type | Severity | Priority | Gender` roles only, never for PII roles. Each table
records its shortest FK path to the Patient table (≤ 3 hops).

Descriptors carry international vocabulary (`ipd`/`opd`, `casualty`/`a&e`/`er`/`ed`, `theatre`/`or`,
`chemist`/`pharmacy`, `nhif`/`shif`/`insurer`, `mpesa`), so common regional naming binds without
intervention.

**Admin overrides.** `PUT /nl2sql/<id>/catalog/overrides` extends the existing aliases and
relationships with `table_concepts` (`{ table, concept | "ignore" }`), `column_roles`
(`{ table, column, role }`) and `service_lines` (`{ table, add, remove }`). Saving rebuilds the
binding and marks affected tables `overridden: true`. `GET /sources/<id>/binding` shows the full
binding, coverage per line, orphans, and a diff against previous versions.

**Usable-line rule.** A service line is usable for a source when ≥ 1 owned concept is bound with
confidence ≥ 0.55; `GET /agents` reports `usable` per line (also true when ingested DocumentDB
`records` contain tables bound to it), the UI hides unusable tabs, and the router never routes to
them. Missing concepts per line are listed in `coverage.lines[line].missing_concepts` so an
administrator can see *why* Maternity is greyed out.

**Alternative schema fixture.** `ontology/tests/alt_schema_binding.json` — 25 tables with different
naming (`PatientMaster`, `Visit`, `IPD_Admission`, `OT_Case`, `Rx`, `RxLine`, `LabReq`, `LabRes`,
`Invoice`, `Receipt`, `Staff`, `Roster`, `Incident`) — must bind ≥ 22/25 concepts correctly, and the
IR golden questions must parse to the same shapes against it
([deterministic-sql-matcher.md §8](deterministic-sql-matcher.md#8-golden-suite-and-acceptance)). A
second full synthetic schema is planned in [plan 08](../new/08-evaluation-and-rollout.md) to guard
against overfitting to the dev seed.
