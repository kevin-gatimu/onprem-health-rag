<!-- markdownlint-disable MD013 -->

# 03a — Implementation brief for plan 03 (QuerySpec IR + deterministic SQL)

**Read `plans/new/03-schema-driven-deterministic-sql.md` first — it is the design.**
This brief is the *delivery contract*: task order, integration anchors verified against the
current code, and six corrections where plan 03 assumes something the codebase does not provide.
Where this brief and plan 03 disagree, **this brief wins**.

**Reconciled against the merged plan-01 code on 2026-09-05** — the type names below are verified,
not assumed. §0.6 lists where plan 01's delivered API differs from what plan 03 was drafted against,
including two accessors plan 03 must add itself. Dispatch only after plan 01's outstanding wiring
(`MetadataOverrides` extension, `refresh_catalog_inner` rebuild hook) has landed, since §4's shadow
mode needs a binding that exists without a manual rebuild call.

Scope: **server only**. Merges behind `ONPREM_SQL_IR_ENABLED=false` (plan 08 §4 step 2) — dual-run,
old templates still answer, IR result logged and compared. Nothing is deleted in this PR.

---

## 0. Corrections to plan 03 (read before writing code)

### 0.1 `RouteEntities` is thinner than the plan assumes — and is model-only

Plan 03 §3 has `parse(q, ents: &RouteEntities, binding, scope)` and describes `RouteEntities` as
carrying "concepts, enum filters, time, metric, dimension, identifiers, top_n". What actually
exists (`router/mod.rs:163`) is:

```rust
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct RouteEntities { pub tables: Vec<String>, pub metric: Option<String>, pub time_bucket: Option<String> }
```

Three fields, `Deserialize` only, and — decisively — it is the output of the **Tier-2 model**
`classify_route` tool call (`RouteToolOutput.entities`), documented as "carried and logged today;
not yet consumed". The richer struct is added by **plan 02** §3.2.

So plan 03's stated dependency ("depends on: 01") is wrong as written: taking a populated
`RouteEntities` makes it depend on 02, which plan 08 sequences *after* it.

**Resolution — plan 03 owns deterministic extraction.** The grammar already has to tokenise the
question and match enum values against `ColumnBinding.enum_values`; that *is* entity extraction.
So:

```rust
pub fn parse(q: &str, hint: Option<&RouteEntities>, binding: &SchemaBinding, scope: &[String]) -> ParseOutcome
```

`hint` is an optional Tier-2 nicety (when present, use `tables` to prefer a subject and
`time_bucket` to seed a bucket); the parse must be fully correct with `None`. Every unit test
passes `None`. Plan 02 later feeds richer hints through the same parameter with no signature
change. Do **not** modify `RouteEntities` in this PR.

### 0.2 `validate_sql` rewrites your SQL — golden fixtures must assert the normalized form

`nl2sql/validate.rs::validate_sql` ends with `ValidatedSql { sql: stmts.remove(0).to_string() }` —
it returns SQL **re-rendered from the sqlparser AST**, not your compiler's string. Whitespace,
quoting, keyword case and parenthesisation are all sqlparser's, not yours.

Therefore plan 03 §9's `expect_sql_pg` / `expect_sql_mysql` / `expect_sql_mssql` fixtures must hold
the **post-`validate_sql`** string. Write the golden generator so it runs `compile → validate_sql`
and asserts on the result. Asserting on raw compiler output produces fixtures that fail the moment
the sqlparser version bumps, and that do not describe what actually executes.

Corollary: do not hand-write the golden SQL. Add a bless helper (env var, e.g.
`ONPREM_BLESS_GOLDEN=1`) that rewrites the fixture from current behaviour, then review the diff.

### 0.3 `compile` needs an injectable "now"

§6 says relative ranges are "inlined as ISO literals computed server-side". If `compile` calls
`Utc::now()` internally, every golden fixture containing *last month / today / within 30 days*
breaks the next day. Signature:

```rust
pub fn compile(spec: &QuerySpec, dialect: SourceKind, max_rows: i64, now: DateTime<Utc>) -> Result<CompiledSql, CompileError>
```

Tests pass a frozen timestamp; production passes `Utc::now()`. Same for whatever resolves
`TimeRange` during parse — thread the reference instant, never read the clock inside a pure function.

### 0.4 `plan_spec` must not use constrained decoding, and needs a flat DTO

§7 proposes a tool call "whose JSON schema *is* `QuerySpec`". The codebase already says twice that
this will fail:

- `FoundryManager::plan_sql` (`foundry/mod.rs:1249`) deliberately bypasses the generic `plan_tool`
  path, commenting: *"Some local ONNX variants cannot compile the `emit_sql` grammar, so the caller
  MUST enforce safety with `nl2sql::validate::validate_sql`"*.
- `plan_list` exists because of *"Foundry's unsupported grammar for arbitrary filter properties"*.
- `extract_clinical_schema` states the convention: *"inlined per array rather than shared via
  `$ref` because small models handle a flat schema far more reliably"*.

`QuerySpec` is much harder than either: nested enums plus a **recursive** `FilterValue`
(`List(Vec<FilterValue>)`, `Range(Box<FilterValue>, Box<FilterValue>)`). It will not survive
constrained decoding, and a recursive JSON schema is precisely what these models fail on.

**Resolution:** a separate flat wire DTO, not `QuerySpec` itself.

```rust
// nl2sql/ir/plan_dto.rs — the model-facing shape. Flat, non-recursive, string-typed values.
pub struct PlannedSpec { subject: String, shape: String, measures: Vec<PlannedMeasure>,
    dimensions: Vec<String>, filters: Vec<PlannedFilter>, time: Option<PlannedTime>,
    order: Option<String>, limit: Option<u32> }
pub struct PlannedFilter { concept: String, role: String, op: String, value: String, values: Vec<String> }
impl TryFrom<PlannedSpec> for QuerySpec { type Error = BindError; /* … */ }
```

Concepts and roles are **enum-constrained strings** in the schema (`"enum": [...]` of slugs);
physical table and column names are forbidden and rejected in `TryFrom`. Route it through the
unconstrained content path with the existing `try_parse_tool` fallback (`foundry/mod.rs:1435`).
Treat `strict: Some(true)` as opt-in only if it proves reliable on `qwen3-8b` — do not depend on it.
A malformed `PlannedSpec` is rejected by `TryFrom` + `bind` before any database sees it, which is
the whole point of §7.

### 0.5 `BLOCKED_FUNCTIONS` is a substring scan over the whole SQL, literals included

`validate_sql` uppercases the entire statement and rejects it if it *contains* `DBCC`, `EXEC(`,
`SLEEP(`, `WAITFOR`, … — including inside string literals. So a compiled filter value can make a
legitimate query unvalidatable. Two consequences:

1. The compiler must never emit those tokens itself (`PERCENTILE_CONT`, `DATE_TRUNC`,
   `TIMESTAMPDIFF`, `DATEDIFF`, `SYSDATETIME` are all fine).
2. When a literal drawn from user text trips it, treat it as a **parse miss** and fall through to
   the planner rather than surfacing a confusing 400. Add one test for this.

### 0.6 What plan 01 actually delivered — verified names, and two accessors you must add

Plan 03 was drafted against a sketch of the binding API. The merged code (`src/ontology/`) differs.
Use these names verbatim:

```rust
// ontology/binding.rs
pub struct SchemaBinding { pub source_id: String, pub bound_at: DateTime<Utc>,
    pub tables: Vec<TableBinding>, pub degraded: bool, pub override_version: u32 }
pub struct TableBinding { pub table_name: String, pub concept: EntityConcept, pub confidence: f32,
    pub service_lines: Vec<ServiceLine>, pub columns: Vec<ColumnBinding>,
    pub patient_path: Option<Vec<JoinHop>>, pub event_time_col: Option<String>, pub degraded: bool }
pub struct ColumnBinding { pub column_name: String, pub role: ColumnRole, pub is_pii: bool,
    pub enum_values: Vec<String> }
pub struct JoinHop { pub from_table: String, pub join_col: String, pub to_table: String, pub via_col: String }
```

Renames from the sketch — all four references in this brief and in plan 03 §3/§5 must be updated:

| Plan 03 says | Delivered | Notes |
|---|---|---|
| `TableBinding::table` | `table_name` | field, not method |
| `SchemaBinding::by_concept` | `table_for_concept(EntityConcept) -> Option<&TableBinding>` | highest-confidence single match |
| `SchemaBinding::tables_for` | `tables_for_concept(EntityConcept) -> Vec<&TableBinding>` | |
| `SchemaBinding::fk_path` | **does not exist** | see below |
| `SchemaBinding::column_with_role` | **does not exist** | see below |

Also present and useful: `usable_lines()`, `patient_table()`, `patient_path_for(&str)`, `coverage()
-> BindingCoverage { total_tables, bound_tables, orphan_tables, exact_concepts, usable_lines }`.
`SchemaBinding` carries **no** `dialect` and no `catalog_version` — `compile` already takes
`dialect: SourceKind` as a parameter (§0.3), so pass it from the source record, not the binding.
`EntityConcept::Unknown` exists and marks orphan tables; `bind.rs` must reject it as a subject.

**Two accessors plan 03 owns.** Add them to `impl SchemaBinding` in `ontology/binding.rs` — pure
accessors over existing data, no binder logic, no scoring change:

```rust
pub fn column_with_role(&self, concept: EntityConcept, role: ColumnRole) -> Option<(&TableBinding, &ColumnBinding)>
pub fn column_with_role_hint(&self, concept: EntityConcept, role: ColumnRole, name_hint: Option<&str>) -> Option<(&TableBinding, &ColumnBinding)>
```

`_hint` prefers a column whose name contains the hint fragment, falling back to the first match —
this is how §3's `name_hint` disambiguates two columns sharing one role (e.g. two `Quantity`
columns). Keep the hint matching case-insensitive and substring-based; it is a preference, never a
requirement, so a binding without the hinted column still resolves.

**The FK gap — do not paper over it.** `TableBinding` retains only the precomputed `patient_path`;
the binder does **not** persist `fk_edges`. So the binding alone cannot produce an arbitrary
concept→concept join path (e.g. `Encounter → Department`). Plan 03 must therefore restrict joins to
what is derivable:

1. `patient_path` for anything that must reach the Patient table.
2. A direct FK between the two bound tables, resolved at bind time by reading `TableCard.fk_edges`
   from the catalog card — `bind()` takes the cards it already needs for nothing else, so give it
   `cards: &[TableCard]` alongside `binding: &SchemaBinding`.
3. Anything else is a `BindError::NoJoinPath` → planner fallback. Assert this in a test rather than
   inventing a join.

If multi-hop non-patient joins turn out to be needed by more than two golden questions, the correct
fix is a follow-up that persists `fk_edges` on `TableBinding` — not a BFS re-derived inside the IR.

---

## 1. Files to create

```
onprem-rag-server/src/nl2sql/ir/
  mod.rs          // re-exports; module docs stating the three-concern split
  spec.rs         // QuerySpec + Shape/Measure/Dimension/Filter/TimeScope/Order/ColumnRef/JoinRef
  parse.rs        // grammar R1-R16, tokeniser, full-consumption check, entity extraction
  predicates.rs   // the §4 domain predicate table, expressed in roles only
  bind.rs         // bind(): concept -> table, role -> column, FK paths, BindError
  compile.rs      // per-dialect SQL + explanation sentence
  plan_dto.rs     // PlannedSpec (§0.4) + TryFrom<PlannedSpec> for QuerySpec
  tests/
    golden.jsonl  // question -> shape + post-validate SQL per dialect, per binding
```

New sibling files (migration, §5): `nl2sql/http.rs`, `nl2sql/prepare.rs`, `nl2sql/text.rs`.

Tests are inline `#[cfg(test)] mod tests`; `ir/tests/golden.jsonl` is a data file loaded with
`include_str!`.

## 2. Order of work

Bottom-up, so each step is independently testable and the PR stays reviewable:

1. `spec.rs` — types only, with serde round-trip tests. No logic.
2. `compile.rs` — against **hand-built** `QuerySpec`s with `physical` already filled. This is the
   most mechanical part and the easiest to test; do it before parse so the grammar has a target.
   Property test from day one: every compile output re-parses in `sqlparser` for its dialect and
   survives `validate_sql`.
3. `bind.rs` — logical `ColumnRef` -> physical, joins from `fk_path` / `patient_path`, `BindError`.
4. `predicates.rs` — the §4 table, each entry resolving through roles + `name_hint` only.
5. `parse.rs` — the grammar last, once a working bind+compile exists to validate against.
6. `plan_dto.rs` + `generate.rs::plan_spec` — the model fallback.
7. Wiring behind the flag (§4). **No deletions.**

## 3. Non-negotiables

- **Zero physical table or column names in `src/nl2sql/ir/**` outside `tests/` and fixtures**
  (plan 03 §10, CI-enforced by plan 08 §3.2). Everything addresses columns as
  `(EntityConcept, ColumnRole, name_hint)`. `name_hint` is a *generic* fragment (`weight`, `expir`,
  `abnormal`, `los`) — never a real dev-seed column name. Verify:
  `rg -n 'patients|encounters|deliveries|prescriptions' onprem-rag-server/src/nl2sql/ir --glob '!tests/*'`
- The same grammar must parse against both `dev_seed_binding.json` and `alt_schema_binding.json`
  (plan 01's fixtures) with the **same `Shape`**, binding to different physical names. A rule that
  only passes on the dev binding is a defect, not partial success.
- Every compiled statement goes through `validate_sql` before execution — no exceptions, including
  the planner fallback.
- PHI stays on-prem: no new network destination. Model calls go through the existing
  `FoundryManager` only.
- Never create `onprem-rag-server/.env`; new keys go in the root `.env.example`.

## 4. Wiring behind the flag

`config.rs`: add `sql_ir_enabled: bool` (`ONPREM_SQL_IR_ENABLED`, **false**) and
`ONPREM_MSSQL_COMPAT_LEVEL` (§6 needs it for the `DATETRUNC` vs `DATEFROMPARTS` split). Use the
existing `env_or` / `env_parse` helpers; add both to `.env.example`.

Anchors (verified line numbers in `nl2sql/routes.rs`):

| Symbol | Line | Change |
|---|---|---|
| `PreparedNlQuery` | 74 | add `pub spec: Option<QuerySpec>`; `explanation` comes from `CompiledSql.explanation` on the IR path |
| `prepare_auto_query` | 368 | unchanged this PR |
| `prepare_auto_query_deterministic` | 383 | unchanged this PR |
| `prepare_with_cards` | 483 | insert the IR attempt ahead of `try_deterministic` when the flag is on; when off, run IR in **shadow** |
| `try_deterministic` | 627 | left in place — it is the dual-run control arm |
| `deterministic_sql` | 678 | untouched this PR |

**Shadow mode (flag off, the default this PR):** run `parse -> bind -> compile -> validate_sql`,
never execute it, and log one structured `ir_shadow` line per question:
`{ parsed, shape, bind_ok, compile_ok, validate_ok, matches_template, elapsed_us }`.
`matches_template` compares the IR's normalized SQL against the template path's normalized SQL
(both post-`validate_sql`, which §0.2 is what makes them comparable). This metric is the evidence
for flipping the flag at golden ≥ 55/60 — it is a deliverable, not a nicety.

Shadow work must never change the answer or fail the request: any error is logged and discarded.

## 5. Migration (do the moves, not the deletions)

Move only — no behaviour change, so the diff stays reviewable:

- HTTP handlers (`nl_query`, `catalog_*`, overrides) -> `nl2sql/http.rs`.
- `prepare_auto_query`, `prepare_auto_query_deterministic`, `prepare_with_cards`,
  `try_deterministic` -> `nl2sql/prepare.rs`.
- `extract_record_identifier`, `contains_likely_person_name`, `normalize_question`,
  `summarize_result`, `render_row` -> `nl2sql/text.rs` (the IR path reuses the first two).

**Keep** `common_healthcare_sql`, `top_grouped_count_join_sql`, `recent_records_subject`,
`periodic_trend_subject`, `patient_overview_subject`, `patient_gender_listing_sql`,
`relationship_filtered_count_sql`, `grouped_count_sql`, `listing_limit`, `deterministic_sql` —
all confirmed present. They are deleted in a later PR once the golden suite passes on the IR path
(plan 03 §8 says "after"; plan 08 §4 step 8 schedules it). Deleting them here removes the control
arm that justifies the flip.

If `nl2sql/routes.rs` does not shrink well below 2,906 lines after the moves, the moves were
incomplete.

## 6. Golden suite

`ir/tests/golden.jsonl`: `{ id, question, binding: "dev"|"alt", expect_shape, expect_sql_pg,
expect_sql_mysql, expect_sql_mssql, expect_missing?: [slot] }`.

- Seed with the 10 A-series questions at their **current** semantics. Because §0.2 normalises and
  §0.3 freezes the clock, byte comparison is now meaningful — but where the template path emits
  something the IR legitimately improves on, assert **result-set equivalence against the live dev
  container** and record why in the fixture, rather than encoding the old SQL as correct.
- Add ≥ 60 questions from `plans/docs/hospital-agents-and-data-map.md` §5. Every *new* example
  there must parse on the dev binding (plan 03 §9).
- Run the whole suite against **both** bindings. Same `Shape` required; SQL differs.
- Property tests: compile output re-parses per dialect; every emitted table ∈ `scope`; no emitted
  identifier is unquoted.
- `Median` on MySQL is expected to return `CompileError` -> planner fallback. Assert that; do not
  work around it.

## 7. Acceptance / gates

```
cd onprem-rag-server && cargo check && cargo test --bin onprem-server
```

- ≥ 55/60 new data-map questions parse+bind+compile on the dev binding; ≥ 50/60 on the alt binding.
- A-series 10/10 unchanged **through the template path** (flag off — proving no regression), and
  10/10 through the IR path in shadow (`matches_template`, or a documented improvement).
- The `rg` grep in §3 returns nothing outside `tests/`.
- Deterministic path p50 < 300 ms on the dev seed, measured with no model call in the path.
- **Do not run `cargo run`** — os error 4551 on this host (Smart App Control). `cargo check` /
  `cargo test` are the verification.

## 8. Out of scope

- Router changes of any kind (Tier 1.5, `RouteEntities` extension, clarify) — plan 02.
- `StructuredExecutor`, `RetrievalFilter`, provenance SSE — plan 04.
- `AgentKind::Line`, personas, linker scope enforcement — plan 05.
- `mutate.rs` / follow-up spec mutation and suggestions — plan 06.
- Bridge and TypeScript — plan 07.
- Deleting the template families — a later PR (§5).
