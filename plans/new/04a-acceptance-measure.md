<!-- markdownlint-disable MD013 -->

# 04a — Acceptance measure for plan 04 (write this BEFORE the executor)

**This document amends `plans/new/04-structured-execution-and-fallbacks.md` §8 and §9.**

**Implementation of plan 04 does not begin until this measure exists in the tree and has been
reviewed.** That ordering is the point. Plan 03 was declared done against an acceptance test that
asserted `sql_pg.is_some()` — that compilation did not *error* — and 52 wrong-SQL defects were found
afterwards, over three correction passes. An acceptance measure written after the code exists gets
shaped by the code. This one is written first so it cannot be.

## §1 — What plan 04 §9 currently measures, and why it is not enough

The three criteria in §9 are structural:

- a grep showing a single call site,
- `provenance.path` **present** on 100 % of persisted messages,
- no path being empty.

All three can hold while every answer the system returns is wrong. They measure that the plumbing was
rewired, which is worth knowing, but they are not an acceptance measure for a component whose job is
to return correct rows to a clinician.

§8's tests are better but still largely mechanism: a state machine over mocked rungs, a spec
round-trip, SSE event ordering. A mocked-rung state machine proves the ladder *branches* as designed.
It cannot prove any rung *answers* correctly, because the rungs are mocks.

## §2 — The measure: executed-result equivalence

Plan 03 verified question → **SQL text**. Plan 04 executes that SQL. Textually-correct SQL can still
return wrong rows: join multiplicity, timezone drift at date boundaries, NULL handling in a rate
denominator, a row cap truncating silently.

Build a results harness alongside the existing golden suite: **question → executed rows**, compared
against expected rows on the dev Postgres seed. Reuse the golden question set — do not invent a
second, easier one — and reuse the frozen `now` (2026-01-15 08:00:00 UTC) so results are
deterministic.

Requirements:

- Compare **values**, not row counts. A count-only assertion passes when the right number of wrong
  rows comes back.
- Order-insensitive comparison unless the question specifies an order (a TopN question does specify
  one — assert its order).
- A question whose correct answer is **zero rows** is a valid expected result and must be asserted as
  such, not skipped. See §5.
- Expected results are derived by reading the seed data and the question, and are reviewed. They are
  not captured from whatever the implementation happens to emit. **Do not add a bless-from-output
  mode to this harness.** Capturing output as expectation is how a false blessing enters, and one has
  already been confirmed in this workstream (a lab-request foreign key blessed as a reviewer column).

This needs the dev Postgres container: `docker compose --profile dev-sources up -d`. That is
localhost — on-prem, no cloud call, no PHI leaving the premises. Gate the live tests behind an env
flag or `#[ignore]` so the default suite stays green without the container, but the harness must be
genuinely runnable, and you must **actually run it once and report the verbatim output**. If the
container cannot start on this host, say so plainly as a limitation — do not simulate a run.

## §3 — Cross-rung agreement: the ladder's untested premise

This is the criterion plan 04 is missing entirely, and the most important one here.

The ladder assumes rung *n+1* is an acceptable substitute when rung *n* misses. For every golden
question that **two or more rungs can answer** (source SQL and DocumentDB aggregation both being able
to count admissions, say), assert that the answers **agree**.

- Where they agree: the fallback is sound for that question, and the ladder has earned the right to
  substitute.
- Where they disagree: that is a **blocking finding**, not a tolerance to configure. Report the
  question, both answers, and the cause. A ladder that silently prefers a disagreeing lower rung is
  worse than one that refuses, because the provenance label reads "we tried the good path first" and
  the user has no way to know the substitution changed the answer.
- Where a lower rung *cannot* answer at all, that is fine and expected — record it as not-comparable,
  not as agreement.

State the count plainly in your report: how many questions are answerable by ≥2 rungs, how many
agree, and every disagreement individually.

## §4 — Row multiplicity

The same hazard as `plans/new/03f-scoped-reverse-fk.md` §2.4, now at execution time where it is
observable. A one-to-many join makes an entity with three child rows count three times, and the
number *looks* plausible.

Assert on seed data where an anchor entity has more than one matching child row that the count is the
number of distinct entities. If no such case exists in the seed, add one to the seed — that is
additive fixture fidelity, not weakening — and say that you did.

## §5 — Empty is an answer, not a failure

Plan 04 §8 already says "no fallback on empty List". Make it an assertion, both ways:

- A correct empty result must be returned as an empty result, with no fallback attempted. Falling
  through to semantic RAG on a legitimately empty structured answer produces a narrative where the
  truthful answer was "none", which is a clinical-safety defect.
- A rung that errors must fall through, and the provenance must carry the `Miss(reason)`.

The distinction between "ran correctly, found nothing" and "failed to run" must be visible in the
`ExecOutcome`, not inferred from an empty vector.

## §6 — Budget skips must not change the answer silently

A rung skipped by `ExecBudget` produces a different answer than one that ran. Assert that a
budget-induced skip appears in `provenance.path` as `Skipped`, and that the response is
distinguishable from the same question answered without the skip. A timeout that quietly downgrades a
clinician's answer from exact rows to a narrative, with no visible marker, is a defect.

## §7 — Provenance must be provably PHI-free

Plan 04 §6 asserts the `provenance` payload is "PHI-free". It is persisted on messages and emitted
over SSE. Nothing enforces the claim.

`Miss("bind error: no EventTime on Bill")` is safe — it names schema, not data. But a reason string
built by interpolating a **filter literal** is not, and filter literals come from the user's question,
which may contain a patient name, a national ID, or an MRN.

Add a test that runs the ladder over questions containing fixture PHI values and asserts that no
`provenance` string — no `Miss` reason, no rung label, no `scope` entry — contains any of them. This
is the same class as the existing `no_pii_enum_values_in_dev_seed` guard, and it must be an
assertion rather than a code comment. A comment documenting a safety property the code does not have
is worse than no comment: one was found in this workstream claiming a bind would refuse when it
actually succeeded with the wrong column.

## §8 — Revised §9 acceptance

Plan 04 is accepted when, in addition to its three structural criteria:

1. The results harness runs on the dev seed and its verbatim output is reported.
2. Every question answerable by ≥2 rungs is either shown to agree, or its disagreement is reported as
   a blocking finding.
3. The multiplicity, empty-vs-failure, budget-skip, and PHI-free-provenance assertions all exist and
   pass.
4. The number of questions returning **verified correct rows** is stated as a number, with the
   remainder accounted for as refused or not-comparable.

Criterion 4 is the headline figure, and it will be lower than plan 03's SQL-verified count. That is
expected and correct — it is a stricter measure of the same pipeline. **A lower number under a
stricter measure is the deliverable.** Reporting plan 03's SQL-verified count as though it were an
executed-result count would be a misrepresentation.

## §9 — The provenance wire format must match the bridge mirror

Plan 04 §6 specifies `Rung` and `RungResult` as data-carrying Rust enums. Under serde defaults
(externally tagged), `Rung::Link(RungResult::Miss("bind error: no EventTime on Bill"))` serialises as:

```json
{"Link": {"Miss": "bind error: no EventTime on Bill"}}
```

The bridge mirror already shipped by plan 07 expects a flat object instead:

```json
{"rung": "Link", "result": "miss", "reason": "bind error: no EventTime on Bill"}
```

**These do not match, and the mismatch fails silently.** `provenance` is `Option` on both sides, so a
shape the client cannot read produces no error — the field is simply absent. Plan 04 §9 asks that
`provenance.path` be present on 100 % of persisted assistant messages; that criterion can read true on
the server while the client renders nothing at all. A structural acceptance criterion cannot catch
this, which is the point of this whole document.

**Decision: the server emits the flat shape.** Reasons: it survives refactors of the internal enum, it
is readable in a log line, it crosses the language boundary without a discriminated-union decoder, and
it is what the client already implements. Do this with a custom `Serialize` or an explicit wire DTO —
keep the internal enum as an enum, because a data-carrying enum is the right Rust for the ladder.

Requirements:

- A serialisation test asserting a `Rung::*(RungResult::Miss(reason))` produces exactly the keys
  `rung`, `result`, `reason` with the exact expected string values, and that `Hit` and `Skipped`
  produce their documented shapes. Assert on the JSON, not on a Rust round-trip through the same
  types — a round-trip passes under any encoding, including a wrong one.
- Verify the field names and letter case against the **actual bridge struct** in
  `onprem-rag-app/src-tauri/src/commands.rs`, not against this document or plan 04's prose. Report the
  file:line you checked.
- If you choose to change the bridge instead of the server, the same requirements apply in reverse,
  both mirrors must be updated together, and say so plainly — but the flat shape is the default and
  needs no justification.

The same caution applies to every other payload plan 04 §6 introduces: `spec`, `suggestions`,
`clarify`. Each has a mirror in `commands.rs` and `bridge.ts` written against this plan's prose while
no server existed to emit it. Before claiming plan 04 complete, confirm each payload against its
mirror on real traffic, and report which ones you verified rather than assuming they line up.

## Everything else still binds

- Never weaken a fixture, rephrase a question, delete a row, or relax an assertion to move a number.
  **A number below the bar is an ACCEPTED deliverable. A number at the bar obtained by editing the
  fixture or the question is a REJECTED deliverable.**
- No question-text special-casing. No literal chosen because it makes one row pass.
- Models propose, Rust decides: every statement executed goes through `nl2sql::validate::validate_sql`,
  the model-SQL rung included. No exception for a fallback path.
- PHI never leaves the premises. The dev Postgres and DocumentDB containers are localhost; no other
  network destination is permitted anywhere in this harness.
- New server response fields must be mirrored in `onprem-rag-app/src-tauri/src/commands.rs` **and**
  `onprem-rag-app/src/lib/bridge.ts`, or they are silently dropped. `provenance` and `spec` on
  persisted messages are new fields — they need both mirrors.
- SSE token payloads stay JSON-encoded; a bare `data:` strips leading spaces and fuses words.
- The bridge holds the JWT in Rust managed state. The web layer never does.
- Do not set `ONPREM_BLESS_GOLDEN`. Do not commit, push, or merge.
- **Do not run `cargo run`** — os error 4551 on this host. Verify with `cargo check` and
  `cargo test --bin onprem-server`.
