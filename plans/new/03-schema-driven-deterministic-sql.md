<!-- markdownlint-disable MD013 -->

# 03 — Schema-driven deterministic SQL

**Goal:** replace the 2,900-line, dev-schema-specific template file with a small grammar that
parses questions into a typed **`QuerySpec` IR**, binds the IR to *any* schema through the
`SchemaBinding` roles (plan 01), and compiles it per dialect. The IR must cover the query shapes a
hospital asks for — counts, sums/averages, rates, group-bys, time buckets, top-N, filtered
listings, lookups, joins along FK paths, durations, occupancy — not the six shapes hardcoded today.

**Depends on:** 01 (`ColumnRole`, `SchemaBinding`). **Unblocks:** 02 (Tier 1.5), 04, 05.

---

## 1. Why an IR

Today `deterministic_sql()` string-matches against `patients`, `encounters`, `deliveries`…, emits
raw SQL per pattern, and is the only place many join shapes exist. Every new hospital schema and
every new question shape means more Rust. An IR separates three concerns that change at different
rates:

| Concern | Changes when | Lives in |
|---|---|---|
| Language → intent (grammar) | new phrasing | `nl2sql/ir/parse.rs` |
| Intent → physical columns (binding) | new hospital | `SchemaBinding` (plan 01) |
| Physical → SQL text (compiler) | new dialect | `nl2sql/ir/compile.rs` |

The same IR is also what the *model* planner is asked to emit as a fallback (§7), what the
DocumentDB aggregation fallback consumes (plan 04), what the UI renders as provenance (plan 07),
and what "and by gender?" follow-ups mutate (plan 06).

## 2. `QuerySpec` (`nl2sql/ir/spec.rs`)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySpec {
    pub subject: Subject,                    // the concept/table rows are drawn from
    pub shape: Shape,
    pub measures: Vec<Measure>,              // empty for List/Lookup
    pub dimensions: Vec<Dimension>,          // GROUP BY
    pub filters: Vec<Filter>,                // AND-ed
    pub time: Option<TimeScope>,             // filter on EventTime/StartTime + optional bucket
    pub order: Vec<Order>,
    pub limit: Option<u32>,
    pub joins: Vec<JoinRef>,                 // resolved at bind time from FK paths
    pub projection: Vec<ColumnRef>,          // List/Lookup display columns
    pub provenance: SpecProvenance,          // which grammar rule, which focus substitutions
}

pub struct Subject { pub concept: EntityConcept, pub table: Option<String> }  // table filled at bind

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape { Scalar, Grouped, Trend, TopN, List, Lookup, Rate, Exists }

pub struct Measure { pub op: MeasureOp, pub target: Option<ColumnRef>, pub alias: String }
pub enum MeasureOp { Count, CountDistinct, Sum, Avg, Min, Max, Median /* dialect-dependent */, Rate { numerator: Box<Filter> } }

pub struct Dimension { pub column: ColumnRef, pub label: String }         // may be on a joined table
pub struct Filter { pub column: ColumnRef, pub op: FilterOp, pub value: FilterValue }
pub enum FilterOp { Eq, Ne, In, Gt, Gte, Lt, Lte, Between, Like, ILike, IsNull, IsNotNull, IsTrue, IsFalse }
pub enum FilterValue { Str(String), Num(f64), Bool(bool), Date(NaiveDate), Ts(DateTime<Utc>), List(Vec<FilterValue>), Range(Box<FilterValue>, Box<FilterValue>), Param(String) }

pub struct TimeScope { pub column: ColumnRef, pub range: Option<TimeRange>, pub bucket: Option<BucketUnit> }
pub struct Order { pub target: OrderTarget, pub dir: SortDir }   // OrderTarget::Measure(alias) | Column(ColumnRef)

/// Logical column: concept + role (+ optional name hint). Resolved to a physical table.column at bind time.
pub struct ColumnRef { pub concept: EntityConcept, pub role: ColumnRole, pub name_hint: Option<String>, pub physical: Option<(String, String)> }

pub struct JoinRef { pub hops: Vec<JoinHop>, pub alias: String, pub kind: JoinKind /* Inner | Left */ }
```

`impl QuerySpec { pub fn intent(&self) -> QueryIntent }` — `Scalar|Grouped|TopN|Rate|Exists →
Aggregation`, `Trend → Trend`, `List → Enumeration`, `Lookup → Lookup`. This keeps the existing
`QueryIntent` contract for the router and UI.

## 3. Grammar (`nl2sql/ir/parse.rs`)

Input: `resolved_question`, `RouteEntities` (plan 02 §3.2 — concepts, enum filters, time, metric,
dimension, identifiers, top_n), `SchemaBinding`, `scope`. Output:

```rust
pub enum ParseOutcome { Parsed { spec: QuerySpec, missing: Vec<MissingSlot> }, NoParse }
pub fn parse(q: &str, ents: &RouteEntities, binding: &SchemaBinding, scope: &[String]) -> ParseOutcome
```

The grammar is a small set of **slot-filling rules**, tried in order; each rule states the tokens
it consumes and the slots it fills. A question parses when a rule matches and *all remaining
tokens* are in a filler allow-list (`the, of, in, for, we, have, are, there, were, our, please,
show, me, tell, give, list, what, is, how, many, much, did, do, does, total, number, records,
all, currently, right, now, please`) — the same "full consumption" idea the existing identifier
name lookup uses, generalised. Rules (with the shape they yield):

| # | Rule (informal) | Shape | Required slots |
|---|---|---|---|
| R1 | `how many|count|number of <concept> [filters] [time] [by <dim>]` | Scalar / Grouped | subject |
| R2 | `total|sum of <Amount/Quantity col> [of <concept>] [filters] [time] [by <dim>]` | Scalar / Grouped | subject, measure target |
| R3 | `average|mean|median|longest|shortest|highest|lowest <Measure/Duration/Amount col> …` | Scalar / Grouped / TopN | subject, target |
| R4 | `<concept> per|by|each <day|week|month|quarter|year> [filters] [time]` or `trend|over time|monthly …` | Trend | subject, EventTime |
| R5 | `which|what|who <dim-concept> had|ordered|prescribed|saw|performed the most|fewest <concept> [time]` | TopN | subject, dimension via FK path |
| R6 | `top|most common|most frequent <N>? <concept|col> [by count] [time]` | TopN | subject |
| R7 | `what is our|the <rate word> …` e.g. "no-show rate", "caesarean rate", "stillbirth rate", "readmission rate", "mortality rate" — rate word ∈ enum value or Flag column | Rate | subject, numerator filter |
| R8 | `list|show|which <concept> [filters] [time] [limit]` / `who is|are <filters>` | List | subject |
| R9 | `tell me about|find|look up|record of <identifier|person name>` | Lookup | Patient/Provider identifier |
| R10 | `is|are there any|do we have <concept> [filters]` | Exists | subject |
| R11 | `<concept> (in|on|at) <Location/Ward/Department value>` as a filter modifier on R1/R8 | — | joins via `*Ref` |
| R12 | `currently|right now|open|active|pending|admitted|in theatre` → status filter from `Status` enum values or `EndTime IS NULL` | — | — |
| R13 | `over|more than|at least <N> <days|hours|minutes>` on a `Duration` column or `EndTime - StartTime` | filter | — |
| R14 | `expires|due|scheduled (within|in the next) <N> <unit>` → `EventTime BETWEEN now AND now+N` | filter | EventTime |
| R15 | `without|missing|no <concept>` → anti-join (`NOT EXISTS`) along FK path | filter | FK path |
| R16 | `low birth weight|category 1|abnormal|critical|late|overdue|unpaid|outstanding` → **domain predicates** table (§4) | filter | — |

Rules are composable: R1 + R11 + R12 + time yields "how many patients are currently admitted in
ICU". Each rule consumes tokens from a token vector with position tracking; a rule fails cheaply
if its anchor token is absent.

**Concept resolution.** A noun phrase maps to a concept when it matches `descriptor.singular /
plural / synonyms`, a bound table name, or an admin alias. Enum values map to filters through
`ColumnBinding.enum_values` across the subject and its one-hop neighbours ("caesarean" → Delivery
→ `delivery_mode = 'caesarean'`; "ambulance" → Encounter → `arrival_mode`). Person-name detection
reuses `contains_likely_person_name`.

**Dimension resolution.** `by ward` → if subject has `WardRef` → join to Ward table's name column
(`Description`/`PersonFullName`-like text column with the highest distinct count ≤ row_count);
else if subject has a `Location` column containing "ward" → that column; else if a `Category`
column's name contains "ward" → it. Same for `provider|doctor|nurse`, `department`, `insurer`,
`medication|drug`, `diagnosis` (→ Code + Description on `Diagnosis`/`DiagnosisCode`).

**Missing slots.** If the rule fired but `subject` could not be resolved (e.g. "how many were
cancelled?") return `Parsed { missing: [Subject] }` so the router can use focus or clarify.

## 4. Domain predicate table (`nl2sql/ir/predicates.rs`)

Fixed, ontology-level definitions expressed in roles — never in physical column names:

| Phrase | Concept | Predicate |
|---|---|---|
| low birth weight | Newborn | `Measure(name_hint: weight) < 2500` (unit from column name `_g` vs `_kg`) |
| stillbirth | Delivery/Newborn | `Status/Category enum ∈ {stillbirth, stillborn}` |
| category 1 / red | Triage | `Measure(name_hint: category) = 1` or `Category enum = red` |
| abnormal / critical | LabResult | `Flag(name_hint: abnormal|critical) IS TRUE` |
| no-show / DNA | Appointment | `Status enum = no_show` |
| outstanding / unpaid | Bill | `Amount(name_hint: due|balance) > 0` or `Status enum ∈ {billed, partial}` |
| readmission | Admission | `Flag(name_hint: readmission) IS TRUE` |
| out of stock | Medication/StockBatch | `SUM(Quantity(name_hint: on_hand)) = 0` (HAVING) |
| expiring within N days | StockBatch/License/BloodUnit | `EventTime(name_hint: expir) BETWEEN now AND now+N` |
| lost to follow-up | ProgramEnrollment | `Status enum = lost_to_followup` |
| break-glass | AccessLog | `Flag(name_hint: break_glass) IS TRUE` |
| in charge | Shift | `Flag(name_hint: in_charge) IS TRUE` |
| pending certification | Mortality | `Identifier/BusinessId(name_hint: certificate) IS NULL` |
| length of stay | Admission | `Duration(name_hint: length|los)` else `EndTime - StartTime` |
| door-to-doctor | Triage | `Duration(name_hint: door_to_doctor)` else `seen_time - arrival_time` |
| theatre time | Surgery | `Duration` else `EndTime - StartTime` |

Each predicate is resolved at bind time; if the roles are absent in the hospital's schema the
predicate is `Unbound` and the parse reports `missing: [Metric]` (router → clarify or planner).

## 5. Binder (`nl2sql/ir/bind.rs`)

```rust
pub fn bind(spec: QuerySpec, binding: &SchemaBinding, scope: &[String]) -> Result<QuerySpec, BindError>
```

- Resolve `Subject.table` = `binding.by_concept(concept)` restricted to `scope`.
- For each `ColumnRef`: same table if role present; else search one-hop FK neighbours (`fk_path`
  ≤ 1) and add a `JoinRef`; else two hops for `Dimension`s only (e.g. Prescription → Encounter →
  Department). `Patient`-scoped filters use `TableBinding.patient_path`.
- `TimeScope.column` defaults to the subject's `EventTime`; "admitted/discharged/booked/scheduled/
  delivered" verbs pick `StartTime`/`EndTime`/`EventTime` by name hint.
- `BindError::{NoSubject, NoRole(role), NoPath(a,b), Ambiguous(role, candidates)}` — `Ambiguous`
  is surfaced as `MissingSlot::Dimension` with the candidate names as clarify options.

## 6. Compiler (`nl2sql/ir/compile.rs`)

```rust
pub fn compile(spec: &QuerySpec, dialect: SourceKind, max_rows: i64) -> Result<CompiledSql, CompileError>
pub struct CompiledSql { pub sql: String, pub params_inlined: bool, pub explanation: String /* human sentence */ }
```

- Identifiers always quoted per dialect (`"x"`, `` `x` ``, `[x]`); aliases `t0..tn`.
- `Count → COUNT(*)`, `CountDistinct → COUNT(DISTINCT t.c)`, `Rate → 100.0 * SUM(CASE WHEN pred THEN 1 ELSE 0 END) / COUNT(*)`; `Median` → `PERCENTILE_CONT(0.5) WITHIN GROUP` (PG/MSSQL) or `NoParse` on MySQL (falls to planner).
- Time buckets: PG `DATE_TRUNC('month', c)`; MySQL `DATE_FORMAT(c, '%Y-%m-01')`; MSSQL `DATETRUNC(month, c)` (2022+) with `DATEFROMPARTS(YEAR(c), MONTH(c), 1)` fallback via `ONPREM_MSSQL_COMPAT_LEVEL`.
- Durations: PG `EXTRACT(EPOCH FROM (e - s))/60`; MySQL `TIMESTAMPDIFF(MINUTE, s, e)`; MSSQL `DATEDIFF(MINUTE, s, e)`.
- `now()`: PG `NOW()`, MySQL `NOW()`, MSSQL `SYSDATETIME()`; relative ranges are inlined as ISO literals computed server-side so plans are cacheable and the SQL is self-contained provenance.
- Values are inlined as properly escaped literals (`'` doubled) — the validator re-parses everything; there is no user text spliced outside a literal.
- `NOT EXISTS` for R15; `LEFT JOIN … IS NULL` avoided for clarity.
- `TopN`: `ORDER BY measure DESC LIMIT n` (`TOP n` on MSSQL, `WITH TIES` when the question says "which … most" and n = 1 to keep the existing tie-preserving benchmark behaviour).
- `List`: projection = `BusinessId`, `PersonGivenName`, `PersonFamilyName`, `EventTime`, `Status`, then up to 4 more non-PII, non-FreeText columns; never `Contact`/`Identifier` roles unless the user is admin **and** asked by name.
- Always `LIMIT max_rows` on non-scalar shapes (validator re-injects anyway).

The `explanation` is a generated sentence used as the SSE `sql.explanation` and for narration:
"Counted deliveries with delivery_mode = caesarean between 2026-08-01 and 2026-08-31, grouped by ward."

## 7. Model fallback speaks the same IR

`nl2sql/generate.rs` gains `plan_spec(...) -> QuerySpec` — a tool call whose JSON schema *is*
`QuerySpec` (concepts and roles as enums, physical names forbidden). Flow in
`prepare_with_cards` becomes: IR parse → (miss) → `plan_spec` → `bind` → `compile` → validate →
(fail) → existing `plan_sql` raw-SQL repair as the last resort. Small local models are far more
reliable filling a typed schema than writing dialect SQL, and a wrong `QuerySpec` is rejected by
`bind` before it ever reaches a database.

## 8. Migration of `nl2sql/routes.rs`

- Move HTTP handlers (`nl_query`, `catalog_*`, overrides) to `nl2sql/http.rs`.
- New `nl2sql/prepare.rs`: `prepare_auto_query`, `prepare_auto_query_deterministic`, `prepare_with_cards`, `try_deterministic` — now calling `ir::parse/bind/compile`.
- Delete `common_healthcare_sql`, `top_grouped_count_join_sql`, `recent_records_subject`, `periodic_trend_subject`, `patient_overview_subject`, `patient_gender_listing_sql`, `relationship_filtered_count_sql`, `grouped_count_sql`, `listing_limit` **after** the regression suite (§9) passes on the IR path. Keep `extract_record_identifier`, `contains_likely_person_name`, `normalize_question`, `summarize_result`, `render_row` (move to `nl2sql/text.rs`).
- `PreparedNlQuery` gains `spec: Option<QuerySpec>` and `explanation` comes from the compiler.

## 9. Tests

- Golden suite `nl2sql/ir/tests/golden.jsonl`: `{ question, binding: "dev"|"alt", expect_shape, expect_sql_pg, expect_sql_mysql, expect_sql_mssql }`. Seed with the 10 A-series questions and their **current** SQL (must stay byte-equivalent modulo whitespace/aliasing, or the assertion is on result set against the live seed), plus ≥ 60 new questions drawn from data-map §5 (every **new** example there must parse on the dev binding).
- Alt-binding tests: the same 60 questions against `alt_schema_binding.json` (plan 01) must parse with the same `Shape` and bind to the alternative names.
- Property tests: compile output always parses in `sqlparser` for its dialect; every emitted table ∈ scope.
- Live: run the golden PG SQL against the dev container and compare row counts to `QUESTIONS.md` ground truth.

## 10. Acceptance

- A-series 10/10 unchanged; ≥ 55/60 new data-map questions answered deterministically on the dev seed; ≥ 50/60 on the alt schema.
- Zero physical table or column names in `nl2sql/ir/*` (CI grep: `patients|encounters|deliveries` must not appear outside tests/fixtures).
- Deterministic path p50 < 300 ms end-to-end on the dev seed (no model call).
