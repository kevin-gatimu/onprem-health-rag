<!-- markdownlint-disable MD013 -->

# 01a — Implementation brief for plan 01 (service-line ontology + schema binding)

**Read `plans/new/01-service-line-ontology-and-schema-binding.md` first — it is the design.**
This brief is the *delivery contract*: task order, the exact integration anchors verified against
the current code, and four corrections where plan 01 assumes something the codebase does not
provide. Where this brief and plan 01 disagree, **this brief wins** (it was written after reading
the code).

Scope: **server only** (`onprem-rag-server/`). No Tauri bridge, no TypeScript — that is plan 07.
Additive: nothing consumes the binding yet except the new endpoints and `GET /agents`
(plan 08 §4 rollout step 1).

---

## 0. Corrections to plan 01 (read before writing code)

### 0.1 No `strum` — hand-write the `ALL` consts

Plan 01 sketches `#[derive(EnumIter)]`. `strum` is **not** in `Cargo.toml` and CLAUDE.md says
"add heavy deps per workstream (not up front)". Do **not** add it. Instead:

```rust
impl ServiceLine {
    pub const ALL: &'static [ServiceLine] = &[ServiceLine::PatientChart, /* … all 13, tier order */];
}
impl EntityConcept { pub const ALL: &'static [EntityConcept] = &[/* … */]; }
impl ColumnRole   { pub const ALL: &'static [ColumnRole]   = &[/* … */]; }
```

Add a test asserting `ALL.len()` equals the variant count and that every `slug()` is unique — that
is what catches a variant added without an `ALL` entry.

### 0.2 PII: there is no persisted per-source PII analysis

Plan 01 §5 assumes stored PII analysis. `POST /schema/analyze` is **stateless** — nothing is
persisted per source. Use the existing single source of truth directly:

```rust
crate::connectors::routes::is_likely_pii(&column.name)   // pub(crate), onprem-rag-server/src/connectors/routes.rs:619
```

Set `ColumnBinding.pii` from it, and treat it as a hard gate: **a `pii == true` column never gets
`enum_values` populated and is never probed.** No exceptions, including when the role looks
categorical.

### 0.3 `enum_values` must come from a bounded probe — `sample_values` holds only 3

`nl2sql/catalog.rs::extract_samples(rows, 3)` caps each column at **3** distinct values
(`if entry.len() >= n { continue; }`). So `sample_values` can never supply the 25 enum values
plan 01 wants. The `SELECT DISTINCT` probe is the **primary** path, not a fallback:

- Candidate: role ∈ {`Status`, `Category`, `Sex`, `Mode`, `Outcome`, or whatever the categorical
  roles end up being}, `!pii`, textual type, and
  `profile.approximate_distinct_count <= config.binding_enum_max` (that field is a *sample-based
  estimate* computed as `(distinct/non_null) * row_count` — treat it as a hint, never as truth).
- SQL: `SELECT DISTINCT <col> FROM <table> WHERE <col> IS NOT NULL LIMIT 26` (dialect-quoted;
  MSSQL uses `TOP 26`). Build it through the existing quoting helpers, push it through
  `nl2sql::validate::validate_sql`, and execute via `nl2sql::execute::run_select`. If it returns
  26 rows the column is not an enum → leave `enum_values` empty.
- **Budget: at most 40 probes per `build_binding` call**, ordered by descending table `row_count`
  then column position, so a 64-table schema stays inside the < 5 s rebuild target. Log the count.
- Every probe failure is non-fatal: warn, leave `enum_values` empty, continue.

### 0.4 Descriptor embeddings are lazy, cached, and optional

Plan 01 §5 says embed the ~60 concept descriptors "once at boot". Don't: it adds boot latency,
makes boot depend on fastembed/ONNX, and breaks `cargo test` on this host (Smart App Control).
Instead:

- Compute on **first** `build_binding` call via `embed::embed_documents`, cache in `AppState`
  (`descriptor_vectors: RwLock<Option<Arc<Vec<Vec<f32>>>>>` or equivalent — one slot, built once).
- If `embed::is_loaded()` is false or embedding errors: **degrade**. Drop the 0.15 embedding term
  and renormalise the remaining four weights to sum to 1.0 (0.35/0.30/0.15/0.05 → ÷0.85), log at
  `warn`, and set a `degraded: bool` (or `embedding_used: bool`) on `SchemaBinding` so the admin
  endpoint can show it.
- Consequence: **every unit test must pass with embeddings absent.** Fixture thresholds
  (≥62/64, ≥22/25) are asserted in the degraded path. If you also want an embedding-on assertion,
  gate it behind `#[ignore]`.

---

## 1. Files to create

```
onprem-rag-server/src/ontology/
  mod.rs            // pub use re-exports; module docs stating the invariants
  service_line.rs   // ServiceLine (13) + label/slug/blurb/tier/concepts/vocabulary/ALL, owner_of
  concepts.rs       // EntityConcept (~60) + ConceptDescriptor table + SHARED_CONCEPTS + ALL
  roles.rs          // ColumnRole (~30) + name-token table + ALL
  binding.rs        // ColumnBinding / TableBinding / JoinHop / SchemaBinding / BindingCoverage + query methods
  binder.rs         // build_binding(): scoring, two passes, roles, enum probe, patient_path, coverage
  store.rs          // persist / load / history in DocumentDB
  routes.rs         // the four endpoints
  tests/
    dev_seed_binding.json    // 64 tables → expected concept + expected service lines
    alt_schema_binding.json  // 25 tables → expected concept + expected service lines
```

Tests are **inline `#[cfg(test)] mod tests`** in the module they test (repo convention — this is a
binary crate, `cargo test --bin onprem-server`). `ontology/tests/*.json` is a *data* directory
loaded with `include_str!`, not a test module.

## 2. Ontology content — source of truth

The concept list and the concept→service-line ownership mapping are **fixed by
`plans/docs/hospital-agents-and-data-map.md` §4**. Transcribe that table verbatim. Do not invent,
merge, rename or drop concepts, and do not reorder the tiers. §5 of the same doc supplies
`ServiceLine::examples()` (needed by `GET /agents`); §8 supplies the binder signals.

**Hard rule (CI-enforced, plan 08 §3.2): no dev-seed physical table name may appear anywhere in
`src/ontology/**` outside `tests/` and the fixture JSON.** `ConceptDescriptor.name_tokens` holds
*generic* tokens (`delivery`, `birth`, `partum`) — never `deliveries`, never `OB_Delivery`. The
same ontology must bind both schemas; if a fixture only passes because a real table name is in the
descriptor, the implementation is wrong. Verify with:

```bash
rg -n 'patients|encounters|admissions|prescriptions|billing_encounters' onprem-rag-server/src/ontology --glob '!tests/*'
```

## 3. Binder algorithm (restated precisely)

Input: the active-version `Vec<TableCard>` for the source (`nl2sql/spec.rs` — `table_name`,
`row_count`, `columns: Vec<CardColumn>{name, type_, nullable, is_primary_key, is_foreign_key,
sample_values, profile}`, `fk_edges: Vec<CardFkEdge>{column, ref_table, ref_column}`, `card_text`,
`card_vector`), plus `MetadataOverrides` for the source.

**Score(table, concept)** = weighted sum, each term normalised to [0,1]:

| Weight | Term |
|---|---|
| 0.35 | table-name token overlap with `descriptor.name_tokens` (singular/plural-folded, `_`/case-split) |
| 0.30 | column-name token overlap with `descriptor.column_tokens` |
| 0.15 | fraction of `descriptor.required_roles` present among the table's assigned roles |
| 0.15 | cosine(`card_vector`, descriptor vector) — **dropped + renormalised** per §0.4 |
| 0.05 | FK-shape agreement (does the table's `fk_edges` point at tables bound to the concept's expected parents) |

**Two passes.** Pass 1 binds the six anchors only — `Patient, Provider, Encounter, Department,
Medication, Ward` — taking the single best-scoring table per anchor concept. Pass 2 binds everything
else, with the anchors' results available so FK-shape and `patient_path` can use them. Within a
pass, assign greedily by descending score; a table binds to at most one concept, and a concept
binds to at most one table per source.

**Ties.** When the top two candidates are within `Δ < 0.05`, prefer the one whose concept belongs
to a service line that currently has **no** usable table. This is what keeps a 25-table schema
spread across lines instead of piling onto Patient Chart.

**Floor.** A `(table, concept)` pair below `config.binding_min_confidence` (0.55) does not bind;
the table lands in `coverage.orphans`.

**`service_lines` per table** = every line that owns the bound concept (a concept can be owned by
more than one line — §4 is the authority), plus every line for a `SHARED_CONCEPTS` concept
(shared concepts are readable by all lines).

### 3.1 Column roles — precedence, highest first

1. Explicit `MetadataOverrides.column_roles` entry.
2. `is_primary_key` → `PrimaryKey`; `is_foreign_key` → the `*Ref` role implied by
   `fk_edges[].ref_table`'s bound concept (e.g. FK to the Patient-bound table → `PatientRef`).
3. Name-token match from `roles.rs` **gated by type compatibility** (a `Money` role needs a numeric
   type; `EventTime` needs a date/timestamp type). Type strings come from the source dialect —
   match case-insensitively on substrings (`int`, `numeric`/`decimal`/`money`, `date`/`time`/
   `timestamp`, `bool`/`bit`, `char`/`text`/`varchar`).
4. Profile-derived: low `approximate_distinct_count` + textual → categorical role;
   `null_ratio == 1.0` → leave `Unknown` (an always-null column is not evidence).
5. Otherwise `Unknown`.

**Exactly one `EventTime` per table.** When several date columns qualify, prefer a
domain-named one (`admit`, `visit`, `order`, `collected`, `delivery`, `service`, `due`) over
`created_at`/`updated_at`/`inserted_at`; among equals take the leftmost column. Demote the rest to
`Timestamp` (or `Unknown`). Downstream time filtering depends on this being unambiguous.

### 3.2 `patient_path`

BFS from the table to the `Patient`-bound table over `fk_edges ∪ MetadataOverrides.relationships`
(both directions), max `config.binding_max_hops` (3) hops, shortest path wins, ties broken by
smaller total hop count then lexicographic table order for determinism. Each hop is a `JoinHop
{ from_table, from_column, to_table, to_column }`. `None` when unreachable or when the table *is*
the Patient table (that is `Some(vec![])`? — no: use `Some(vec![])` for the Patient table itself
and `None` for unreachable, and document the distinction in a doc comment, because plan 05 §3
tests `patient_path.len() <= 2`).

### 3.3 Coverage

```rust
pub struct BindingCoverage {
    pub bound_tables: usize, pub total_tables: usize,
    pub usable_lines: Vec<ServiceLine>,          // ≥1 owned concept bound at ≥ min_confidence
    pub unusable_lines: Vec<ServiceLine>,
    pub missing_concepts: Vec<EntityConcept>,    // owned by some line, bound nowhere
    pub orphans: Vec<String>,                    // tables with no concept / no line
}
```

`usable_lines()` on `SchemaBinding` is the rule the UI and `GET /agents` key off: **a line is
usable when ≥1 concept it owns is bound with confidence ≥ `ONPREM_BINDING_MIN_CONFIDENCE`.**

## 4. Persistence (`store.rs`)

Add to `documentdb/mod.rs`, following the existing constant + accessor pattern exactly:

```rust
pub const SCHEMA_BINDINGS: &str = "schema_bindings";
pub const SCHEMA_BINDING_HISTORY: &str = "schema_binding_history";
// + the two Collection<Document> accessor fns alongside the existing ones
```

- `schema_bindings`: one doc per `source_id` (upsert on `{source_id}`), current binding +
  `built_at`, `catalog_version`, `degraded`.
- `schema_binding_history`: append each build; **keep the last 5 per source**, delete older
  (mirror how `refresh_catalog_inner` prunes stale catalog versions).
- Index: `{source_id: 1}` on bindings, `{source_id: 1, built_at: -1}` on history — created in the
  existing `nl2sql::catalog::ensure_nl2sql_indexes` (or an `ontology::store::ensure_indexes` called
  from the same boot path; pick one and be consistent).
- Serialise with `mongodb::bson::to_document` on the `SchemaBinding` serde impl. `f32` confidences
  → BSON doubles; make sure round-trip through `from_document` is tested (one unit test:
  build a small `SchemaBinding` by hand, `to_document` → `from_document`, assert equality).

## 5. Integration anchors (verified — use these, don't search for your own)

| File | Change |
|---|---|
| `src/main.rs` | add `mod ontology;` to the existing `mod` block; mount the four routes in the existing grouped `.mount("/", rocket::routes![…])` — put them with the nl2sql/connector group, not with `agents::routes::agent`. |
| `src/state.rs` | add `bindings: RwLock<HashMap<String, Arc<SchemaBinding>>>` + `descriptor_vectors` slot (§0.4). Accessors: `binding(&self, source_id) -> Option<Arc<SchemaBinding>>`, `set_binding(...)`, `any_binding_for(&self, line) -> Option<Arc<SchemaBinding>>`. Follow the existing private-field + accessor style used for `catalog`/`foundry`. `AppState::new` signature stays unchanged — initialise the map empty. |
| `src/nl2sql/catalog.rs::refresh_catalog_inner` | after the `active_version` flip and `linker::invalidate_source(source_id)`, call `ontology::binder::build_binding` → `store::save` → `state.set_binding`. **Non-fatal**: on error `tracing::warn!` and continue; a failed binding must never fail a catalog refresh. Note `refresh_catalog_inner` may not currently hold `&AppState` — thread what you need in rather than reaching for a global. |
| `src/aggregation/catalog.rs::build_from_store` | augment `CollectionMeta` with `concept: Option<EntityConcept>` and `service_lines: Vec<ServiceLine>` from the binding when one exists. Both fields default-empty so nothing existing breaks; do **not** add a `Catalog::scoped` yet (plan 05). |
| `src/config.rs` | add to the "Schema metadata maintenance" section at the end of `Config`: `binding_min_confidence: f32` (`ONPREM_BINDING_MIN_CONFIDENCE`, 0.55), `binding_enum_max: usize` (`ONPREM_BINDING_ENUM_MAX`, 25), `binding_max_hops: usize` (`ONPREM_BINDING_MAX_HOPS`, 3), `binding_enabled: bool` (`ONPREM_BINDING_ENABLED`, **true** — plan 08 §4 step 1). Use the existing `env_or` / `env_parse` helpers. Mirror all four into `.env.example` with comments. |
| `src/nl2sql/routes.rs` | extend `MetadataOverrides` (§6). |
| `src/connectors/routes.rs` | no change — call `is_likely_pii`, `connected_source_ids`, `load_spec` (all `pub(crate)`). |

## 6. `MetadataOverrides` extension

```rust
pub struct MetadataTableConcept { pub table: String, pub concept: Option<String> }  // None = ignore this table
pub struct MetadataColumnRole  { pub table: String, pub column: String, pub role: String }
pub struct MetadataServiceLines { pub table: String, pub service_lines: Vec<String> }

pub struct MetadataOverrides {
    #[serde(default)] pub aliases: Vec<MetadataAlias>,
    #[serde(default)] pub relationships: Vec<MetadataRelationship>,
    #[serde(default)] pub table_concepts: Vec<MetadataTableConcept>,   // new
    #[serde(default)] pub column_roles: Vec<MetadataColumnRole>,       // new
    #[serde(default)] pub service_lines: Vec<MetadataServiceLines>,    // new
}
```

`#[serde(default)]` on every new field is what keeps existing stored override documents loading.

Extend `validate_overrides` (same function, same style) to additionally reject: unknown table,
unknown column, unparseable concept slug, unparseable role name, unparseable service-line slug.
Reuse the `HashMap<String, HashSet<String>>` table→columns map it already builds from the active
catalog version. After a successful save, the existing `linker::invalidate_source` call stays, and
you add a binding rebuild (non-fatal, same treatment as §5).

The existing `audit::write_audit(db, &user.id, &user.username, "schema_metadata_overrides_updated",
source_id, Some(json!({…counts})))` call must gain counts for the three new arrays. Binding
rebuilds get their own audit action `schema_binding_rebuilt`.

## 7. Endpoints

| Route | Auth | Returns |
|---|---|---|
| `GET /agents` | any authenticated user | `Vec<AgentInfo>` — Ask first, then the 13 lines in tier order. `usable` from `usable_lines()` across **all connected sources** (`connected_source_ids`). `sources: [{source_id, tables}]`, `example_questions` filtered to those whose required concepts are bound, `modes: ["ask","trends","handover"]`, `tier`, `label`, `blurb`, `kind` (slug; Ask = `"ask"`). |
| `GET /sources/<id>/binding` | **admin** | full `SchemaBinding` + `coverage` (incl. `orphans`, `degraded`). |
| `POST /sources/<id>/binding/rebuild` | **admin** | rebuild now, persist, update state, audit; returns the new binding. |
| `GET /sources/<id>/binding/history` | **admin** | last 5 entries, newest first (metadata + coverage; no need to inline every table). |

Return `Result<T, AppError>` (repo convention). Unknown source → `AppError::NotFound`. Source with
no catalog yet → `AppError::BadRequest("no schema catalog for source; refresh the catalog first")`.
`GET /agents` when no source is bound must still return the full roster with `usable: false`
everywhere — the UI needs the roster to render greyed tabs, never an error.

Guard reads/writes with `binding_enabled`: when false, `GET /agents` returns Ask only and the three
admin endpoints return `AppError::Unavailable`.

## 8. Fixtures and tests

`ontology/tests/dev_seed_binding.json` — the **64** tables in
`docker/dev-postgres/init/01_schema.sql`, each with its expected `concept` and expected
`service_lines`. Confirmed table list (use exactly these names):

```
admissions allergen_catalog antenatal_visits appointments bed_assignments beds
billing_encounters billing_items blood_units care_programs cds_alerts clinical_notes
consents counties deliveries departments diagnoses drug_interactions encounters
equipment equipment_maintenance icd10_codes imaging_orders immunizations
incident_reports insurance_claims insurance_providers lab_orders lab_results
lab_test_catalog medication_administration medication_catalog mortality_records
newborns notifiable_disease_reports patient_allergies patient_documents
patient_family_history patient_feedback patient_insurance patient_medical_history
patient_transfers patients payments prescription_items prescriptions procedure_codes
procedures program_enrollments provider_licenses provider_schedules provider_shifts
provider_time_off providers record_access_log referrals stock_batches stock_movements
surgeries transfusions triage_assessments vaccine_catalog vital_signs wards
```

`alt_schema_binding.json` — the ~25 alt-schema tables from `plans/new/08-evaluation-and-rollout.md`
§1 (`PatientMaster`, `Visit`, `IPD_Admission`, `Booking`, `OB_Delivery`, `OB_Baby`, `Rx`, `RxLine`,
`LabReq`, `LabRes`, `Invoice`, `Receipt`, `Staff`, `Roster`, `Incident`, …). PascalCase, prefixes,
`DeptCode` FK, no `patient_no`.

The fixtures describe **expected output**, so they cannot be generated from a live DB in a unit
test. Build synthetic `TableCard`s from the two schemas (a small helper that turns a compact
`table → [(column, type, pk?, fk→table.col)]` literal into `TableCard`s with empty `card_vector`
and default profiles), run `build_binding`'s pure scoring path against them, and compare.
**Factor the pure scoring out of the async DB-touching `build_binding`** so this is possible:

```rust
pub(crate) fn bind_cards(cards: &[TableCard], overrides: &MetadataOverrides,
                         cfg: &BinderParams, descriptors: Option<&[Vec<f32>]>) -> SchemaBinding
```

`build_binding` = load cards → `bind_cards` → enum probes (the only DB/network part) → coverage.
Only `bind_cards` is unit-tested; it must be sync and dependency-free.

Assertions:

1. Dev seed: **≥ 62 / 64** tables get the exact expected concept; **64 / 64** get a
   `service_lines` set that is a superset of the expected set (never a wrong line, extra allowed
   via shared concepts). `coverage.orphans` is empty — the "no orphans" assertion plan 05 §3 reuses.
2. Alt schema: **≥ 22 / 25** exact concepts; `usable_lines().len() >= 9`.
3. `ALL` completeness + unique slugs for all three enums.
4. `patient_path`: `vital_signs`→`patients` ≤ 2 hops on dev; `LabRes`→`PatientMaster` resolves on alt.
5. Exactly one `EventTime` per bound table, and `created_at` is never it when a domain date exists.
6. No `ColumnBinding` with `pii == true` has non-empty `enum_values` (assert across both fixtures).
7. `SchemaBinding` BSON round-trip.
8. `validate_overrides` rejects an unknown table, an unknown column, a bad concept slug, a bad role,
   a bad line slug — and accepts a valid one.
9. Overrides win: a `table_concepts` override forces the concept even when scoring disagrees;
   `concept: None` removes the table from every line.
10. Determinism: `bind_cards` on a shuffled card order produces an identical `SchemaBinding`
    (sort inputs where needed — this is the test that catches HashMap iteration leaking into output).

## 9. Gates before you report done

```bash
cd onprem-rag-server && cargo check && cargo test --bin onprem-server
```

Plus:

- `cargo clippy --bin onprem-server` clean for new code if clippy is available (don't fight
  pre-existing warnings elsewhere).
- The `rg` grep in §2 returns nothing outside `tests/`.
- **Do not run `cargo run`** — it fails on this host with os error 4551 (Smart App Control).
  `cargo check` / `cargo test` are the verification.
- No new network destination: the only outbound calls you add are through the existing
  `nl2sql::execute::run_select` (source DBs) and `embed::embed_documents` (local fastembed).
- `git status` must show no new `onprem-rag-server/.env`. Real secrets live in the **root** `.env`.

## 10. Out of scope — do not touch

- `src-tauri/` and `onprem-rag-app/src/` (plan 07 mirrors the new fields; doing it now would leave
  half-wired UI).
- `nl2sql/linker.rs` scoping, `Catalog::scoped`, `RetrievalFilter`, `AgentKind::Line`,
  `QuerySpec`/IR, router v3 (plans 02–05). Add the ontology types they will need, nothing more.
- Deleting or rewriting existing template families in `nl2sql/routes.rs`.
- `plans/docs/` updates (a separate pass handles documentation).

Report back with: files added/changed, the two fixture pass rates (x/64 and x/25), the
`cargo check` + `cargo test` output, and any place you deviated from this brief and why.
