<!-- markdownlint-disable MD013 -->

# 03h — Turn the audit findings into harness invariants

**Read `plans/new/03b-defect-closure-brief.md` §1 and §5 first.** Its invariant governs: *a spec must
express everything the question stated, or the parse must refuse.*

**Sequencing: last in plan 03 — after `03e`, `03f`, `03g`.** It asserts over the emitted SQL of every
golden row, so it must run against the final fixture and the final rule set. Running it earlier would
produce findings that the remaining plans invalidate.

## Why

Every defect found in this workstream was found by a **human reading SQL**. Not one was caught by the
harness:

- 52 wrong-SQL rows passed an acceptance test that asserted `sql_pg.is_some()` — that compilation did
  not error.
- Four rows emitted `IN` lists of enum labels that do not exist, for three consecutive passes,
  because the fixture's `enum_values` were empty and the guard that would have caught it was dead code.
- A `name_hint` miss silently fell back to the first column of the right role, blessing SQL that
  filtered a lab-request foreign key as a reviewer column, and a PII column as a death certificate.
- A predicate carried a comment claiming it would refuse. It did not refuse. Nothing compared the
  comment to the behaviour.

The common shape: **the harness could see everything it needed to catch these, and was not asked to
look.** Each defect class below is mechanically detectable from data the harness already has. This
plan converts the audit heuristics that found them into assertions, so the class cannot silently
return once the humans who remember it move on.

An assertion that would not have caught the original defect is not worth writing. For each invariant
below, confirm it fires on the historical case before you trust it — the brief says how.

## §1 — A row that should refuse, and does not, must be reported

`dx-01-alt` and `qs-03` now carry `expect_sql_pg: null` because **refusal is the correct outcome** —
both were false blessings, and neither is marked `defect: true`. But a row with null expectations is
currently *skipped*, not *checked*. If a future change makes either emit SQL again, the harness stays
green and the false blessing returns unobserved.

Assert: for any row whose SQL expectations are null and which is not marked `defect: true`, the
pipeline emitting SQL is a **failure**. Report the row id, the question, and the emitted SQL.

This is the same asymmetry that produced the original 52: the harness checked that expected SQL
appeared, and never checked that unexpected SQL did not.

## §2 — A predicate that cannot exclude any row is a silent no-op

The question asked for a restriction; the SQL applied none; the result set looks like a plausible
answer to a broader question. Nothing errors.

Detect and fail on:

- `IS NOT NULL` on a column the DDL declares `NOT NULL`. Confirmed in this workstream by a temporary
  probe, which was then deleted — make it permanent.
- `IN (...)` over a set that covers **every** label the bound column's domain allows.
- A range or threshold predicate with no effective bound.

After 03e the fixture carries nullability and enum labels derived from the real DDL, so this is
decidable from the binding rather than guessable from the SQL string. Prefer inspecting the IR and the
column metadata over regexing the emitted text.

A no-op predicate is **not** the same as a filter that matches nothing — see §3. This invariant is
about a clause that *cannot* discriminate, not one that happens to select zero rows.

## §3 — Every literal must exist in the bound column's domain

Postgres treats these two cases very differently, and the difference decides which is worse:

- On an **ENUM** column, comparing against a label that does not exist is a **runtime error**. Loud.
- On a **`VARCHAR ... CHECK (col IN (...))`** column, a non-existent literal is silently `false`. It
  matches nothing and returns an empty result that is indistinguishable from a truthful "none".

The second is the dangerous one, and it is the one a text-comparison harness cannot see. Assert that
every literal in an emitted `=` or `IN` comparison exists in the bound column's derived label set.

Where a column genuinely has an open domain (free text, an identifier, a numeric measure), the
invariant does not apply — but **enumerate those columns explicitly** and say why each is exempt. An
exemption list you can read is a control; a silent skip is the hole this plan exists to close.

## §4 — A `name_hint` miss must refuse (regression guard)

`column_with_role_hint_in_table` in `src/nl2sql/ir/bind.rs` previously fell back to the first column of
the matching role when a `name_hint` was supplied and matched nothing. That single fallback produced
two false blessings and two latent mis-bindings. It has been fixed: a hint is now a **requirement**,
and a miss returns `None` so the bind fails and the pipeline refuses.

Nothing stops it being reintroduced. The refusals it causes are locally inconvenient — each one looks
like a row that "should" work — and that is exactly the pressure that produced the fallback the first
time.

Add a direct unit test: a predicate whose `name_hint` matches no column in the table binds to `None`
and the parse refuses. Assert the refusal, not just the `None`. Reference this section in a comment at
the call site so the next person to consider a fallback finds the reason it is forbidden.

## §5 — Scope and honesty about what these will find

Run every invariant over **all** golden rows, not only blessed ones. An invariant that runs only on
rows with expected SQL cannot see the refusing rows, which is where §1's whole subject lives.

These assertions may fail on rows currently counted `verified`. **That is the expected outcome and the
point of the plan.** If one fires:

- **Report it. Do not fix it, and do not adjust the row to make it pass.** A newly-detected wrong row
  is a finding I need to see, not a number to restore.
- Do not lower any ratchet. You are not authorised to lower a floor in this pass. If `verified` drops
  because a row was found to be wrong, stop and report — that drop is the deliverable.

If every invariant passes on the first run, say so plainly, and then demonstrate each one actually
works by confirming it fires on the historical case it was written for (§4's fallback, §3's
non-existent label, §2's `IS NOT NULL` on a `NOT NULL` column, §1's null-expectation row emitting SQL).
Use a temporary local mutation to prove it, and revert that mutation. **An invariant that has never
been observed to fail is indistinguishable from one that cannot fail** — and this workstream has
already shipped one of those: a guard asserting over `mortality_records.national_id`, a column that
does not exist.

## Everything else still binds

- Never weaken a fixture, rephrase a question, delete a row, or relax an assertion to move a number.
  **A number below the bar is an ACCEPTED deliverable. A number at the bar obtained by editing the
  fixture or the question is a REJECTED deliverable.**
- No question-text special-casing. No literal chosen because it makes one fixture row pass.
- Do not rename fixture tables or columns — 03e owns the fixture's physical names.
- Zero physical table/column names in `src/nl2sql/ir/**` or `src/ontology/**` outside `#[cfg(test)]`
  and fixtures. The exemption list in §3 lives with the fixtures, not in the rule code.
- PHI stays on-prem: no network destination, no live database connection in tests.
- Do not set `ONPREM_BLESS_GOLDEN`. Do not commit, push, or merge.
- **Do not run `cargo run`** — os error 4551 on this host. Verify with `cargo check` and
  `cargo test --bin onprem-server`. `ONPREM_GOLDEN_DUMP_DEFECT_SQL=1 cargo test --bin onprem-server
  golden_suite -- --nocapture` is the primary diagnostic.

## Report

- Each invariant: where it lives, what data it reads, and whether it inspects the IR or the emitted SQL.
- For each of the four: the historical defect it was written for, and confirmation it fires on that
  case — including the temporary mutation you used and that you reverted it.
- Every row any invariant fired on: id, question, emitted SQL, and which invariant. Reported, **not**
  fixed.
- The §3 exemption list: every open-domain column, and why each is exempt.
- Verbatim: full suite count and the `verified / wrong / refused` line for both bindings.
