<!-- markdownlint-disable MD013 -->

# 03c — Enum fidelity + criticality role (closes 8 of 10 remaining defects)

**Read `plans/new/03b-defect-closure-brief.md` first. Every non-negotiable in its §5 remains in force.**

Entry state, verbatim from `cargo test --bin onprem-server golden_suite -- --nocapture`:

```text
golden_suite_dev_binding: 28/62 SQL-verified | 6 wrong (defect) | 28 refused
  Defect ids: fd-04, wb-01, mat-04, dx-01, rev-02, wf-01
golden_suite_alt_binding: 22/62 SQL-verified | 4 wrong (defect) | 21 refused
  Defect ids: dx-01-alt, wb-01-alt, fd-04-alt, mat-04-alt
```

The ratchets in `ir/mod.rs` (`verified >= 26/20`, `wrong <= 31/21`) must be **raised**, never lowered.

---

## Task 1 — Wire real enum labels into the golden harness

`src/nl2sql/ir/mod.rs:76` calls
`bind_cards(cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new())`.

That final argument is `table -> column -> labels`. Passing an empty map means **every**
`ColumnBinding.enum_values` in the golden suite is empty, so the `BindError::UnsatisfiableFilter`
intersection added in the previous pass can never fire. It is dead code in this suite. That single
argument is why four defect rows still emit SQL.

Add a fixture enum map and pass it for both bindings. Labels are **ground truth copied verbatim from
`docker/dev-postgres/init/01_schema.sql`** — do not invent, extend, or trim a label set:

| Table | Column | Labels | Source |
|---|---|---|---|
| `appointments` | `appointment_status` | `booked, confirmed, checked_in, in_progress, completed, cancelled, no_show, rescheduled` | line 31 `appointment_status` ENUM |
| `beds` | `status` | `available, occupied, cleaning, maintenance, blocked` | line 33 `bed_status` ENUM |
| `deliveries` | `delivery_mode` | `spontaneous_vaginal, caesarean, vacuum_assisted, forceps, breech` | line 805 CHECK |
| `insurance_claims` | `status` | `submitted, under_review, approved, partially_approved, rejected, paid` | line 757 CHECK |
| `medication_administrations` | `status` | `given, held, refused, missed, self_administered` | line 35 `admin_status` ENUM |
| `prescriptions` | `status` | `active, dispensed, partially_dispensed, cancelled, expired` | prescriptions CHECK |
| `staff_shifts` | `shift_type` | `day, evening, night, on_call, long_day` | line 940 CHECK |
| `encounters` | `encounter_type` | `outpatient, inpatient, emergency, follow_up, telehealth, daycase` | line 28 ENUM |
| `referrals` | `status` | `pending, accepted, scheduled, completed, declined, expired` | line 32 ENUM |

Mirror the same **logical label sets** onto the corresponding alt-binding tables and columns. The alt
schema is a rename-only variant of the same hospital domain (03a §3), so the label values are
identical; only table and column names differ. Map them by concept + role, not by string.

**Why this is a permitted fixture edit** (§5 forbids weakening a fixture): supplying true labels can
only *narrow* an emitted `IN` list or *cause a refusal*. It cannot widen a filter or make a wrong
answer pass. It moves the fixture toward the production schema. If you find yourself wanting to
*remove* a label so a row passes, stop — that is the forbidden direction, and it means the parse is
wrong, not the label set.

### Expected effect, and the audit you must do

These rows should narrow to exactly one label each:

- `fd-04` `IN ('no_show','dna','did_not_attend')` → `IN ('no_show')`
- `wb-01` `IN ('available','free','vacant')` → `IN ('available')`
- `mat-04` `IN ('caesarean','c_section','lscs','cs')` → `IN ('caesarean')`
- `rev-02` `IN ('rejected','denied')` → `IN ('rejected')`

Plus `fd-04-alt`, `wb-01-alt`, `mat-04-alt`.

If a row narrows as above **and** you have read its question and confirmed the SQL answers it, set
`expect_sql_pg` / `_mysql` / `_mssql` and clear `"defect": true`. Quote question + final SQL for each
in your report.

Confirm `wf-02` still emits `shift_type IN ('night')` and stays verified — `night` is a real label, so
it must survive. If any currently-verified row changes SQL, **report it and stop**; do not re-bless it.

## Task 2 — Criticality is not abnormality (closes dx-01, dx-01-alt)

`dx-01` "Which lab results are critical and unreviewed?" emits:

```sql
SELECT t0."result_no", t0."result_date" FROM "lab_results" AS t0
WHERE t0."is_abnormal" IS TRUE AND t0."verified_by" IS NULL LIMIT 500
```

`01_schema.sql:641-643` carries **three distinct columns**: `is_abnormal BOOLEAN`,
`abnormal_flag VARCHAR(3)`, and `is_critical BOOLEAN`. Answering "critical" with `is_abnormal`
over-reports — it returns every mildly-abnormal result as critical. This is the clinical-safety class
03b exists to close, and it is worse than a refusal.

Two parts:

1. Add a criticality `ColumnRole` in `src/ontology/roles.rs` alongside the existing abnormality role,
   and give it binder recognition (`is_critical`, `critical`, `critical_flag`) distinct from
   abnormality. Do not let either role's matcher swallow the other's column names.
2. Add `col("is_critical", "boolean", false, false)` to the `lab_results` fixture card (dev) and its
   alt counterpart. This is the same ground-truth-fidelity addition as Task 1 — the real table has
   this column and the fixture omitted it.

`dx-01` must then emit `is_critical IS TRUE AND verified_by IS NULL`. **`dx-04` "Were there any
abnormal results this week?" must keep `is_abnormal IS TRUE`** — that question says *abnormal*. These
two rows are the discriminating pair; if a change makes both use the same column, the roles are not
actually distinct and the fix is wrong. Verify both.

If criticality cannot be bound for a binding, **refuse** — do not fall back to abnormality.

## Task 3 — Bless wf-01 (already audited correct)

```sql
SELECT t0."license_no", t0."issued_on", t0."provider_id" FROM "provider_licenses" AS t0
WHERE t0."expires_on" >= '2026-01-01' AND t0."expires_on" < '2026-04-01' LIMIT 500
```

Question: "Whose practising licence expires this quarter?" It filters `expires_on` (the Family D bug
is fixed), the Q1 2026 bounds are correct from frozen now = 2026-01-15, and `provider_id` makes
"whose" answerable. Set its three `expect_sql_*` fields and clear `"defect": true`.

`wf-01-alt` correctly carries `expect_missing: ["License"]` — the alt fixture binds no licence table.
Leave it alone.

## Hard constraints

- **Do not touch the physical table or column *names* in the fixture.** A separate drift list
  (`staff_shifts` vs `provider_shifts`, `appointment_status` vs `status`, `result_date` vs
  `resulted_at`, and ~10 more) is pending a decision from Kevin, because renaming invalidates blessed
  SQL wholesale. Task 1 and Task 2 are *additive* only: new enum labels, one new column. Nothing
  renamed.
- Never lower a ratchet, weaken a fixture, rephrase a question, or relax an assertion. **A number
  below the bar is an ACCEPTED deliverable; a number at the bar obtained by editing the fixture or the
  question is a REJECTED deliverable.**
- No question-text special-casing. No literal chosen to make one fixture row pass.
- Zero physical table/column names in `src/nl2sql/ir/**` or `src/ontology/**` outside `#[cfg(test)]`
  and fixtures.
- No PII column may carry `enum_values` — `no_pii_enum_values_in_dev_seed` guards this; keep it green.
- Every compiled statement still goes through `nl2sql::validate::validate_sql`.
- Do not set `ONPREM_BLESS_GOLDEN`. Do not commit, push, or merge.
- **Do not run `cargo run`** (os error 4551 on this host). Verify with `cargo check` and
  `cargo test --bin onprem-server`.
- Do not implement reverse-FK traversal — 03a §0.6 forbids it; the cohort family stays refusing.

## Report

Verbatim test output: full suite count, and the `verified / wrong / refused` line per binding. State
which ids moved `wrong -> verified` and which moved to `refused` (a move to `refused` is progress, not
a regression). Quote question + final SQL for every row you blessed. After this pass `wrong` should be
at or near 0/0; say plainly which rows remain and why.
