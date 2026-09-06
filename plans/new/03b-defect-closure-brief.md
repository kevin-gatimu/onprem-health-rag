<!-- markdownlint-disable MD013 -->

# 03b — Defect closure brief for plan 03 (IR correctness)

**Read `plans/new/03a-implementation-brief.md` first.** This brief closes the gap that 03a §6's
golden suite was supposed to catch and did not.

**Status as measured on 2026-09-05**, after the golden suite was made to compare shape and SQL:

| Binding | Total | SQL-verified | Audited-wrong | NoParse | `expect_missing` |
|---|---|---|---|---|---|
| dev | 62 | **26** | **31** | 5 | 0 |
| alt | 62 | **20** | **21** | 6 | 15 |

The previously-reported 57/62 and 56/62 measured `sql_pg.is_some()` — that compilation did not
error. It never compared the SQL. 31 dev rows compile to SQL that answers **a different question
than the one asked**, and did so while green.

This is a clinical-safety defect class, not a quality shortfall. *"How many beds are free?"*
compiles to `SELECT COUNT(*) FROM "beds"`. *"How many patients are on TB treatment?"* compiles to
`SELECT COUNT(*) FROM "patients"`. The user receives a plausible number with nothing indicating the
cohort filter was never applied.

`ONPREM_SQL_IR_ENABLED=false` and `ONPREM_ROUTER_V3=false`, so none of this is live. It must not go
live, and **plan 04 (`StructuredExecutor`) is blocked** until the `wrong` count below reaches 0 —
plan 04 is what executes these specs against real records.

---

## 1. The governing invariant

**A spec must express everything the question stated, or the parse must refuse.**

Refusing is safe: `ParseOutcome::NoParse` falls through to the planner and then the semantic path,
which answers with grounded passages instead of a confident wrong number. Emitting an approximation
is not safe at any confidence.

This was enforced twice already — the R13 duration threshold and the domain-predicate path, both in
`nl2sql/ir/parse.rs`. Each fix traded a green golden row for an honest miss. That is the correct
trade and it is the pattern for everything below.

Corollary for every fix in this brief: when a constraint cannot be carried, **return `None` from the
rule**. Do not widen a role lookup, do not substitute a nearby column, do not drop the clause.

## 2. Gate reframing — read before touching the harness

The `assert!(passed >= 55)` / `>= 50` bars in `ir/mod.rs` count rows where the pipeline emitted
*some* SQL. The audit proves that quantity is not correctness — and worse, the bar **rewards
emitting SQL for questions the pipeline cannot answer**, the exact behaviour §1 forbids. Applying §1
will push `passed` down as wrong answers become honest refusals.

So replace the single bar with three counters, each with a direction:

| Counter | Meaning | Direction |
|---|---|---|
| `verified` | audited-correct SQL: blessed and byte-matched | **ratchets up**, never down |
| `wrong` | rows marked `"defect": true` — compiles, but answers a different question | **ratchets down to 0** |
| `refused` | pipeline declines; planner/semantic answers instead | reported, unbounded |

Retire the `passed >= 55` / `>= 50` asserts and state in a comment why: they measured compilation,
the audit showed compilation is not correctness, and the bar paid for approximation. `verified >= 26`
(dev) / `>= 20` (alt) plus `wrong <= 31` / `<= 21` are strictly stricter than what they replace —
every fix moves a row from `wrong` to `verified`, so both bars tighten together.

**This is the one place in this brief where a bar is removed, the reasoning is given, and the
replacement is stronger. It is not licence to adjust any other threshold anywhere.**

## 3. Defect families and the row worklist

### Family A — wrong subject concept (13 dev rows)

The parse picks a plausible-but-unrelated table and answers from it. The most dangerous family: the
SQL is well-formed and the data is real, just not the data asked for.

`pc-03` `pc-04` `em-01` `em-03` `th-01` `th-02` `th-04` `dx-03` `qs-02` `qs-04` `mat-05` `cc-03` `wf-03`

Examples: *"How many surgeries were performed this month?"* → `patient_documents`. *"How many
patients arrived in the emergency last month?"* → `patients`. Triage wait, surgery duration and lab
turnaround all → `AVG(length_of_stay_days) FROM admissions`. *"Which doctor saw the most patients
last month?"* → counts providers by `hire_date`, with no join to encounters at all.

**Expected resolution: refuse.** Where the concept the question names is not bound, subject
resolution must return `None`, not fall back to the nearest bound table. Diagnose *why* the fallback
is taken — a confidence floor set too low, a substring match on the wrong axis, a duration role that
matches any concept. Fix the general rule; do not enumerate these ids in code.

### Family B — filter recognised but not carried (9 dev rows)

Right table, missing or spurious predicate.

`fd-04` `wb-01` `mat-04` `ph-05` `rev-02` `dx-01` `fac-02` `mat-06` `wb-02`

Missing: no-show status, bed availability, `delivery_type = 'caesarean'`, `status = 'refused'`,
`status = 'rejected'`, the unreviewed half of *"critical and unreviewed"*, overdue servicing.
Spurious: `wb-02` *"Who is admitted right now?"* adds `admitted_at = today`, excluding everyone
admitted earlier; `mat-06` adds `discharge_date IS NULL`, excluding discharged newborns.

**Expected resolution: carry it, or refuse.** The most tractable family — the enum value is present
in `ColumnBinding.enum_values` for most of these. A spurious constraint is as wrong as a missing one.

### Family C — cohort membership needing a join (5 dev rows)

`cc-01` `cc-02` `cc-04` `gen-03` `gen-04`

TB treatment, HIV programme, chronic-care enrolment, type-2 diabetes, missed last appointment — all
reduce to `COUNT(*) FROM patients` or the unfiltered patient list.

**Expected resolution: refuse**, unless the join is expressible within 03a §0.6's two permitted
forms (`patient_path`, or a direct forward FK from `TableCard.fk_edges`). Most of these need a
*reverse* FK traversal — `diagnoses.patient_id → patients.id` read backwards — which §0.6 does not
permit. If reverse-FK traversal is the blocker for most of the family, **say so and stop**: that is
a scoped follow-up (persisting `fk_edges` on `TableBinding`), not something to improvise here.

### Family D — wrong column role on the right table (4 dev rows)

`wf-01` `ph-04` `wf-02` `dx-04`

`wf-01` *"Whose practising licence expires this quarter?"* filters `issued_on`, so it returns
newly-issued licences. `ph-04` sums `quantity_on_hand` of medications *expiring* in the window
instead of quantity dispensed. `wf-02` *"Who is on shift tonight?"* projects only `shift_date` — no
staff identifier, so the "who" is unanswerable from the result. `dx-04` drops the "this week" bound.

**Expected resolution: fix role selection.** `wf-01` and `dx-04` are genuine role/time bugs.
`ph-04` is a measure the schema may not carry — refuse if so.

### Alt binding

The 21 alt rows are the same questions under different physical names, so a general fix moves both.
Any fix that moves dev and not alt is not general — 03a §3 already states that a rule which only
passes on the dev binding is a defect, not partial success.

## 4. Order of work

1. **Harness and fixture only** (`ir/mod.rs`, `ir/tests/golden.jsonl`): add `"defect": true` to the
   52 audited-wrong rows, implement §2's three counters, retire the two `passed` bars. No behaviour
   change — the numbers must come out 26/31 dev and 20/21 alt.
2. **Family A diagnosis before Family A code.** Report the root cause of the fallback first. One
   general fix probably closes most of the family; 13 individual patches would be the wrong shape.
3. Family B, then D — both local and low-risk.
4. Family C last, and only after reporting whether reverse-FK traversal is the blocker.

## 5. Non-negotiables

- **Never** lower a threshold, weaken a fixture, delete or rephrase a row, relax an assertion, or
  add a skip to make a number move. §2 is the sole, argued exception. **A number below the bar is an
  ACCEPTED deliverable. A number at the bar obtained by editing the fixture, blessing wrong SQL, or
  loosening the harness is a REJECTED deliverable.**
- **No question-text special-casing.** No string match on any fixture question or a fragment of one.
  A fix that names its own row is rejected.
- **Zero physical table or column names** in `src/nl2sql/ir/**` outside `tests/` and fixtures.
- Every compiled statement still goes through `nl2sql::validate::validate_sql` — no exceptions,
  planner fallback included.
- Do not re-bless a row without auditing its SQL against its question first. Blessing is how wrong
  behaviour becomes the expectation.
- PHI stays on-prem: no new network destination, no live database connection in tests.
- **Do not run `cargo run`** — os error 4551 on this host. `cargo check` and
  `cargo test --bin onprem-server` are the verification.
- Do not commit, push, or merge.

## 6. Reporting

Every pass reports, verbatim from test output: full suite count, `verified` / `wrong` / `refused`
per binding, and the `[dev]` / `[alt]` router accuracy lines. State which ids moved from `wrong` to
`verified` and which moved to `refused`. **A row that moved to `refused` is progress, not a
regression** — say so plainly rather than trying to recover the number.
