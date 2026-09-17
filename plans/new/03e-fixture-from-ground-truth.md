<!-- markdownlint-disable MD013 -->

# 03e — Derive the dev fixture from ground truth

**Read `plans/new/03b-defect-closure-brief.md` §1 and §5 first, then `plans/new/03d-fixture-drift-audit.md`.**
03d is the evidence base for this plan; do not re-derive its findings, but do verify any specific line
number you rely on.

## Why

`dev_seed_cards()` in `src/ontology/tests/mod.rs` is ~800 lines of hand-written table cards that no
process reconciles against the real schema. 03d measured the result: **18 of 28 blessed dev rows
(64%), 54 SQL cells, reference tables or columns that do not exist in
`docker/dev-postgres/init/01_schema.sql`.** The golden suite has been certifying SQL that would error
on the real database.

Two failures already traced to this fixture:

- Its `enum_values` were empty everywhere, so the `BindError::UnsatisfiableFilter` guard was dead code
  and four defect rows kept emitting `IN` lists of non-existent labels for three passes.
- It mistook a *type* name for a *column* name (`appointments.appointment_status`; the real column is
  `status`, the ENUM type is `appointment_status`). A hand-written fixture can make that error. A
  derived one cannot.

The goal is not to correct 24 names. It is to remove the possibility of drift.

## Approach

Preferred: **`dev_seed_cards()` parses the real DDL at test time.**
`include_str!` the schema file and build the `TableCard` list from it, so there is no second copy of
the truth and nothing to keep in sync. Verify the relative path depth yourself rather than trusting a
guess — the file is `src/ontology/tests/mod.rs` and the target is
`docker/dev-postgres/init/01_schema.sql` at the repo root.

The parser needs to handle only what this schema uses: `CREATE TYPE ... AS ENUM (...)`,
`CREATE TABLE`, column name + type, `PRIMARY KEY`, `REFERENCES <table>(<col>)`, `NOT NULL`, and
`CHECK (<col> IN (...))`. Derive `enum_values` from both `CREATE TYPE ... AS ENUM` (resolved through
the column's declared type) and inline `CHECK (... IN (...))` lists — that is where Task 1 of 03c was
getting its labels by hand, and here they come for free and cannot be wrong.

If DDL parsing proves genuinely unreliable, the fallback is: generate the fixture once with a script,
commit the output, and add a **conformance test** that re-parses the DDL and asserts the fixture still
matches it. State which route you took and why. The conformance property is the non-negotiable part —
a corrected-but-still-hand-written fixture is not an acceptable outcome, because it drifts again.

Guard the parser against silently producing a wrong fixture:

- Assert the parsed table count matches the number of `CREATE TABLE` statements in the file.
- Assert every table has at least one column and a primary key.
- Assert a few known specifics as canaries, e.g. `beds.status` carries exactly the five `bed_status`
  labels, and `lab_results` carries `is_abnormal`, `abnormal_flag` and `is_critical` as distinct
  columns.
- Keep `no_pii_enum_values_in_dev_seed` green. PII columns must still carry empty `enum_values`, and
  PII classification must now be derived from real column names — note that the audit found the
  fixture asserting over an invented `mortality_records.national_id`, so this guard has been testing a
  column that does not exist.

## Do not touch the alt fixture

03d concludes `alt_schema_cards()` is a deliberate adversarial variant that stresses the binder's name
normalisation, not a model of the real schema. Its names are correct by construction. It has no drift.
Leave it exactly as it is — and note this means the alt binding is your control: **if a rule breaks on
alt after this change, the rule was depending on dev's physical names, which is itself a defect.**

## Re-blessing — 18 rows, and three of them are not renames

Most of the 18 are mechanical: the query is right, the identifier was wrong. Regenerate, observe the
new SQL, confirm it answers the question, re-bless.

But the audit found columns that are **absent entirely**, not renamed: `admissions.admission_no`,
`admissions.admission_status`, and `mortality_records.death_no` / `national_id` / `completed_at`. A row
whose blessed SQL projects or filters one of those cannot be re-blessed with a substitute column. For
each such row, either the question is answerable from columns that genuinely exist — in which case
show the new SQL — or **it must refuse**, per 03b §1: a spec must express everything the question
stated, or the parse must refuse. Do not substitute a nearby column to keep a row green. Substituting
`admission_date` for an invented `admission_status` is exactly the Family B defect this workstream
exists to eliminate.

## The ratchet — this is the one authorised exception, and it is NOT yours to exercise

Regeneration will invalidate blessed SQL and push `verified` below the `>= 26` (dev) assert in
`ir/mod.rs` until re-blessing is complete.

**Leave the ratchet literals exactly as they are.** Let the assert fail. The counters print before the
assertion, so the numbers are still visible in the output — report them. The project owner has
authorised lowering this specific floor once, with evidence, and will do it himself after reviewing
your re-blessing. An agent lowering it is indistinguishable from an agent hiding a regression, so it
is rejected on sight.

Every other threshold in the repo stays exactly where it is. This exception does not generalise.

## Everything else still binds

- Never weaken a fixture, rephrase a question, delete a row, or relax an assertion to move a number.
  **A number below the bar is an ACCEPTED deliverable. A number at the bar obtained by editing the
  fixture or the question is a REJECTED deliverable.**
- No question-text special-casing. No literal chosen because it makes one fixture row pass.
- Zero physical table/column names in `src/nl2sql/ir/**` or `src/ontology/**` outside `#[cfg(test)]`
  and fixtures.
- Every compiled statement goes through `nl2sql::validate::validate_sql`.
- PHI stays on-prem: no network destination, no live database connection in tests. Parse the DDL file
  from disk — do not connect to Postgres to introspect it.
- Do not set `ONPREM_BLESS_GOLDEN`. Do not commit, push, or merge.
- **Do not run `cargo run`** — os error 4551 on this host. Verify with `cargo check` and
  `cargo test --bin onprem-server`. `ONPREM_GOLDEN_DUMP_DEFECT_SQL=1 cargo test --bin onprem-server
  golden_suite -- --nocapture` dumps defect SQL and is your primary diagnostic.

## Report

- Which route you took (test-time parsing vs generate-once + conformance test) and why.
- Verbatim: full suite count, and the `verified / wrong / refused` line for both bindings — including
  the failing ratchet assert.
- For each of the 18 rows: question, old SQL, new SQL, and whether you re-blessed it or it now refuses.
- Any row that changed on the **alt** binding — that would indicate a rule was depending on dev's
  physical names, and is a finding in its own right.
- Anything the DDL parser could not handle, stated plainly as a limitation rather than worked around.
