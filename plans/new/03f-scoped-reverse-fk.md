<!-- markdownlint-disable MD013 -->

# 03f — Scoped single-hop reverse-FK traversal (closes the cohort family)

**Read `plans/new/03b-defect-closure-brief.md` §1 and §5 first.** Its governing invariant binds every
decision here: *a spec must express everything the question stated, or the parse must refuse.*

**Sequencing: this plan runs AFTER `plans/new/03e-fixture-from-ground-truth.md` is complete and
reviewed.** Both edit `golden.jsonl` and `src/nl2sql/ir/bind.rs`. Do not start until 03e's regenerated
fixture is in the tree, and build on the regenerated physical names rather than the ones any older
document quotes.

## Why this plan exists, and why it was previously forbidden

`plans/new/03a-*.md` §0.6 permits exactly two join forms: the canonical `patient_path`, or a direct
**forward** foreign key read off `TableCard.fk_edges`. That restriction was deliberate — unconstrained
join inference is how NL→SQL systems invent relationships and answer confidently with the wrong join
graph.

The cost is a whole family of refusals. Cohort questions name an anchor entity but state the
distinguishing condition on a *child* table: "how many patients are on TB treatment", "patients in the
HIV programme", "chronic care patients", "patients with type-2 diabetes", "patients who missed
appointments". The anchor is `Patient`; the condition lives in a table whose FK points **at** the
patient. Forward traversal cannot reach it. Six golden rows refuse for this single reason.

The project owner has authorised reverse traversal **scoped**, not opened. §0.6 is amended by this
document to permit a third join form under every constraint in §2 below. Anything not explicitly
permitted here remains forbidden.

## §1 — The IR needs a real sub-scope, not a flattened filter

A cohort condition is not a filter on the anchor table. Flattening it into `QuerySpec.filters` and
hoping the binder resolves the column elsewhere is exactly the class of silent mis-binding this
workstream has been closing all week.

Represent it explicitly. Shape it as a related-entity scope carrying its own concept and its own
filters, with negation as a field rather than a separate variant — for example a `related: Vec<...>`
on `QuerySpec` where each element holds the related `EntityConcept`, that concept's filters, and a
`negated: bool`. Name it as you see fit; the requirements are that (a) the relationship is a typed
structure in the IR, not a string or a pre-joined column reference, and (b) `validate_sql` and the
golden diff can both see that a reverse hop was taken.

Negation matters because R15 (anti-join) is the same mechanism inverted: "patients who missed
appointments" and "patients with no follow-up booked" differ only in polarity. Implement both
polarities in one code path — two paths will drift.

## §2 — The scope, stated as hard constraints

Every one of these is a refusal condition, not a warning. A violated constraint means the parse
refuses; it never means "pick something reasonable".

1. **Single hop.** At most one reverse edge from the anchor. No reverse chains, and no reverse edge
   followed by a forward edge. Two hops refuse.
2. **The reverse edge must reference the anchor's primary key.** A foreign key pointing at a non-PK
   column is not an identity relationship. Refuse.
3. **Exactly one candidate child table, resolved by concept — never by position.** If more than one
   table reverse-references the anchor and could satisfy the bound concept, the traversal is
   ambiguous and **must refuse.** Do **not** take the first candidate.
   This is not a hypothetical: a defect was confirmed in `column_with_role_hint_in_table` where a
   `name_hint` miss silently fell through to the first column of the right role, blessing SQL that
   filtered a lab-request foreign key as though it were a reviewer column. Reproducing that pattern
   at table granularity would be strictly worse. If you find yourself writing `first`, `next()`, or
   an unconditional `[0]` over a candidate list, that is the bug.
4. **Counting across a reverse edge must not multiply rows.** A reverse edge is one-to-many by
   construction. A plain `INNER JOIN` makes a patient with three TB treatment records count three
   times, and the result *looks* plausible — this is the highest-risk defect in the whole feature.
   Compile the semi-join as `EXISTS (SELECT 1 FROM child WHERE child.fk = anchor.pk AND <filters>)`,
   and the anti-join as `NOT EXISTS` over the same shape. If you have a reason to prefer a join plus
   `DISTINCT` instead, prove per dialect that the count is unchanged and say so explicitly; absent
   that proof, use `EXISTS`.
   Add a test that would fail under the multiplying form — a fixture anchor row with two matching
   child rows, asserting the count is 1. Without that test this constraint is decorative.
5. **The child table must be independently readable.** Reverse traversal must not become a way to
   reach a table the requesting ServiceLine's allow-list does not cover. Apply the same ownership
   check that a directly-bound table gets. Ownership maps remain read allow-lists, **not** a security
   boundary — RBAC stays in `auth/`; do not move or duplicate any authorisation logic here.
6. **Prefer forward.** If the question is expressible via `patient_path` or a forward FK, use that.
   Reverse traversal is a fallback, and preferring it would change already-verified SQL.
7. **All three dialects.** Postgres, MySQL, SQL Server. `EXISTS` is portable; verify the emitted text
   per dialect rather than assuming.
8. Every compiled statement still goes through `nl2sql::validate::validate_sql`. The validator must
   understand the new construct — a subquery it cannot parse must be rejected, never waved through.

## §3 — Plumbing: `fk_edges` must survive binding

`TableCard` carries `fk_edges`, but the binding layer drops them, so `TableBinding` cannot answer
"which tables point at me". Persist the edges through `bind_cards` onto `TableBinding` and build the
reverse index from the bound set only — never from unbound cards, or traversal would reach tables the
confidence gate excluded.

Build the reverse index once per binding rather than scanning every card per query.

## §4 — Expected effect and the honest-refusal cases

The six cohort rows are the target. Do not assume all six close.

- Where the child table and its filter column both bind, the row should now emit `EXISTS` SQL. Read
  the question, confirm the SQL answers it, then bless and quote both in your report.
- Where the alt fixture has no corresponding child table, the row must refuse — carry
  `expect_missing: [...]` with the unbound concept, matching how `wf-01-alt` and `rev-02-alt` already
  handle absent concepts.
- Where the condition needs a value that does not exist as data (a "chronic care" flag that is a
  clinical judgement rather than a column, say), the row **refuses**. Do not approximate it with a
  nearby column. An honest refusal is an accepted deliverable.

Report each of the six separately with its outcome. "Closed 6 of 6" without per-row evidence will be
treated as unverified.

## §5 — Ratchets

`verified` may be **raised** to the newly measured value once you have audited each blessed row.
`wrong` must remain 0 on both bindings — if this feature makes any row emit incorrect SQL, that is a
blocking defect in the feature, not a number to accommodate.

You are **not** authorised to lower any floor in this pass. If a floor drops, stop and report; a drop
here would mean the feature broke a previously-verified row, which I need to see rather than have
absorbed into a threshold.

## Everything else still binds

- Never weaken a fixture, rephrase a question, delete a row, or relax an assertion to move a number.
  **A number below the bar is an ACCEPTED deliverable. A number at the bar obtained by editing the
  fixture or the question is a REJECTED deliverable.**
- No question-text special-casing. No phrase or literal chosen because it makes one fixture row pass.
- Do not rename fixture tables or columns — 03e owns the fixture's physical names.
- Zero physical table/column names in `src/nl2sql/ir/**` or `src/ontology/**` outside `#[cfg(test)]`
  and fixtures.
- PHI stays on-prem: no network destination, no live database connection in tests.
- Do not set `ONPREM_BLESS_GOLDEN`. Do not commit, push, or merge.
- **Do not run `cargo run`** — os error 4551 on this host. Verify with `cargo check` and
  `cargo test --bin onprem-server`. `ONPREM_GOLDEN_DUMP_DEFECT_SQL=1 cargo test --bin onprem-server
  golden_suite -- --nocapture` is the primary diagnostic.

## Report

- The IR structure you added, and how a reverse hop is visible to `validate_sql` and the golden diff.
- The emitted SQL shape per dialect for one semi-join and one anti-join.
- The row-multiplication test: what it asserts, and confirmation it fails if `EXISTS` is swapped for a
  plain join.
- Each of the six cohort rows: question, outcome (blessed with SQL quoted / refused with reason).
- Any ambiguous-candidate case you hit, and that it refused rather than guessing.
- Verbatim: full suite count and the `verified / wrong / refused` line for both bindings.
- Any previously-verified row whose SQL changed — reported, not re-blessed.
