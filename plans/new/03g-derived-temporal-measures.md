<!-- markdownlint-disable MD013 -->

# 03g — Derived temporal measures (length of stay, turnaround)

**Read `plans/new/03b-defect-closure-brief.md` §1 and §5 first.** Its invariant governs: *a spec must
express everything the question stated, or the parse must refuse.*

**Sequencing: after `03e` (fixture regeneration) and `03f` (reverse FK).** All three edit
`golden.jsonl`. Build on the regenerated fixture, not on physical names quoted in older documents.

## Why

After 03e shrinks the unbound-concept refusals and 03f closes the cohort family, the remaining
refusals are questions whose measure is not stored in any column — it is the **difference between two
timestamps**:

- **Length of stay** — discharge minus admission.
- **Lab turnaround** — result time minus collection or order time. Golden rows `wb-04-alt`,
  `dx-03-alt` refuse for this reason.

These are among the most-requested hospital metrics, so the gap is worth closing. But a timestamp
difference is the single most dialect-divergent construct in this whole workstream, and the failure
modes all produce numbers that look plausible. Read §2 before writing any compiler code.

## §1 — Represent the measure in the IR, with its unit

Add a derived-measure construct to `QuerySpec`: an expression over two `ColumnRole`-bound temporal
columns plus an explicit **unit** (days, hours, minutes).

The unit is not optional and not inferred at compile time. Length of stay is conventionally days;
turnaround is conventionally hours. Getting the unit wrong is a 24× error that reads as a plausible
number — "average turnaround 1.8" is believable as hours and as days, and only one is right. Carry
the unit in the IR, and make the emitted column alias name it (`avg_los_days`, `avg_turnaround_hours`)
so a wrong unit is visible in the output rather than silent.

The measure must compose with existing shapes: projected on a List, aggregated on a Scalar or
Grouped, and thresholded. If R13 (duration threshold) currently computes a duration by its own
separate route, unify them onto this construct — two implementations of the same arithmetic will
drift, and one of them will be the one nobody tested.

## §2 — The cross-dialect trap: native day-diff functions do not agree

Do not emit each dialect's native day-difference function and assume equivalence. They compute
different things.

- **SQL Server** `DATEDIFF(day, a, b)` counts **boundary crossings**, not elapsed time. Admitted
  23:00 Monday, discharged 01:00 Tuesday returns **1** — two hours of stay reported as one day.
- **Postgres** timestamp subtraction yields an interval; the same case is ~0.083 days.
- **MySQL** `DATEDIFF(a, b)` counts date boundaries like SQL Server, while `TIMESTAMPDIFF` counts
  whole elapsed units and truncates.

So the same question against the same data returns different answers per dialect. That is not
acceptable in a system whose entire premise is that Rust decides.

**Normalise to elapsed seconds, then divide by the unit.** Emit the per-dialect epoch/second
difference (Postgres `EXTRACT(EPOCH FROM (b - a))`, and each other dialect's equivalent
second-granularity difference) and divide by 86400, 3600, or 60. All three dialects then agree.

Add a test with a case that **would expose the divergence** — an interval that crosses midnight but
spans only a couple of hours — asserting all three dialects produce the same value. A test using a
clean multi-day interval passes under every one of the wrong implementations and is therefore
worthless here.

### Argument order is a sign-flip waiting to happen

`DATEDIFF` argument order differs between MySQL (`end, start`) and SQL Server (`unit, start, end`).
Reversing them yields a **negative** length of stay. Assert positivity on a known-good fixture row in
every dialect — a sign flip must fail a test, not reach a clinician.

## §3 — Open intervals: the statistical trap

A currently-admitted patient has a NULL discharge timestamp. Their length of stay is not zero and it
is not missing — it is *ongoing*.

This must be an explicit, visible decision, never a silent consequence of SQL semantics. `AVG` skips
NULLs, so a naive implementation silently computes "average stay of patients who have already left",
which is **biased short** precisely because long-staying patients are still there. That is a wrong
number delivered confidently, which is the class this workstream exists to eliminate.

Choose per question intent and record the choice in the IR:

- **Completed only** — exclude open intervals. Defensible, and the honest default for "average length
  of stay", but the answer must say so.
- **As of now** — treat an open interval as ending at the frozen `now`. Correct for "how long have
  current inpatients been here", which is a different question.

Whichever a row uses, the narration must state it. Do not let the two questions compile to the same
SQL. If the question does not disambiguate and both readings give materially different answers,
refuse per 03b §1 rather than picking one.

Negative durations indicate a data-quality problem in the source. Do **not** silently filter them out
— that hides a real defect in the hospital's data behind a clean-looking average. The measure must not
crash on them; surfacing them is correct.

## §4 — Refuse when the second timestamp is not bound

Both endpoints must bind to real temporal columns via their roles. If a binding has an admission
timestamp but no discharge timestamp, length of stay is **not computable** and the row refuses.

Do not substitute a nearby temporal column. This is the exact defect already confirmed in this
workstream, where a `name_hint` miss silently fell back to the first column of the right role and
blessed SQL filtering a lab-request foreign key as though it were a reviewer column. A "temporal
column of roughly the right kind" is the same mistake with a plausible number attached instead of a
plausible row set.

## §5 — Ratchets

`verified` may be **raised** to the newly measured value once you have audited each blessed row.
`wrong` must remain 0 on both bindings.

You are **not** authorised to lower any floor. If one drops, stop and report — that would mean this
feature broke a previously-verified row, which I need to see rather than have absorbed.

## Everything else still binds

- Never weaken a fixture, rephrase a question, delete a row, or relax an assertion to move a number.
  **A number below the bar is an ACCEPTED deliverable. A number at the bar obtained by editing the
  fixture or the question is a REJECTED deliverable.**
- No question-text special-casing. No literal chosen because it makes one fixture row pass.
- Do not rename fixture tables or columns — 03e owns the fixture's physical names.
- Zero physical table/column names in `src/nl2sql/ir/**` or `src/ontology/**` outside `#[cfg(test)]`
  and fixtures.
- Every compiled statement goes through `nl2sql::validate::validate_sql`; the validator must
  understand the new expression rather than waving it through.
- PHI stays on-prem: no network destination, no live database connection in tests.
- Do not set `ONPREM_BLESS_GOLDEN`. Do not commit, push, or merge.
- **Do not run `cargo run`** — os error 4551 on this host. Verify with `cargo check` and
  `cargo test --bin onprem-server`. `ONPREM_GOLDEN_DUMP_DEFECT_SQL=1 cargo test --bin onprem-server
  golden_suite -- --nocapture` is the primary diagnostic.

## Report

- The IR construct, and how the unit and the open-interval choice are represented.
- The emitted SQL per dialect for one length-of-stay and one turnaround question.
- The cross-dialect agreement test: the interval it uses, why that interval exposes the divergence,
  and confirmation it fails if a native day-diff is substituted.
- The positivity assertion, per dialect.
- Each row that moved from refused to verified: question and SQL, quoted.
- Any row that must still refuse, and which endpoint failed to bind.
- Verbatim: full suite count and the `verified / wrong / refused` line for both bindings.
