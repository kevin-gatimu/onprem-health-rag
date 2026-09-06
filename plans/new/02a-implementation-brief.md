<!-- markdownlint-disable MD013 -->

# 02a — Implementation brief for plan 02 (Intent router v3)

**Status: implemented.** Router v3 ships behind `ONPREM_ROUTER_V3`, currently defaulting to off, so
rollout is pending while the implementation itself is done. The "today"/"currently" descriptions and
line numbers below record the state of the code **before** this brief was implemented — they are
deliberately preserved as history and must not be edited to match the current tree. The current code
is the authority on where things live; find things by identifier name, not by the line numbers here.

**Read `plans/new/02-intent-router-v3.md` first — it is the design.**
This brief is the *delivery contract*: task order, integration anchors verified against the
current code, and nine corrections where plan 02 assumes something the codebase does not provide.
Where this brief and plan 02 disagree, **this brief wins**.

**Reconciled against merged plans 01 + 03 on 2026-09-05** — every type name and line number below
was read from the tree, not assumed.

Scope: **server only**. Merges behind `ONPREM_ROUTER_V3=false` (plan 08 §4 step 3) — v2 `route()`
still decides every request; v3 runs only when the flag is on. Nothing is deleted in this PR.

---

## 0. Corrections to plan 02 (read before writing code)

### 0.1 `ConversationFocus` (06) and `AgentMode` (05) do not exist — and ship *after* this plan

Plan 02 §4's `RouteRequest` carries `focus: &ConversationFocus` (plan 06) and `mode: AgentMode`
(plan 05). Neither type exists. Plan 08 §4 sequences this plan **before** both. This is the same
defect class as 03a §0.1 (plan 03 declared "depends on 01" but required plan 02's types).

**Resolution — plan 02 owns a minimal `ConversationFocus`; plan 06 extends it.** §3.1 focus
resolution and §8's clarify tests both need a real focus value, so a stub is not enough:

```rust
// router/focus.rs — plan 02 owns this minimal shape. Plan 06 POPULATES it from persisted
// conversation state and may ADD fields; it must NOT redefine or relocate it.
#[derive(Debug, Clone, Default)]
pub struct ConversationFocus {
    pub concept: Option<EntityConcept>,
    pub patient_key: Option<String>,
    pub time_range: Option<TimeRange>,   // the IR's TimeRange — see §0.7
    pub line: Option<ServiceLine>,
    pub last_spec: Option<QuerySpec>,
}
```

`Default` is the "no focus" case, and every v3 unit test constructs focus explicitly. In this PR
nothing populates it from the database — the call sites pass `&ConversationFocus::default()`, so
§3.1's substitutions are exercised only by tests until plan 06 wires persistence. Say that in the
module doc so plan 06's author knows the type is already there.

**`AgentMode`: omit entirely.** No §3 logic reads it — it is pass-through for plan 05. Leave the
field out of `RouteRequest`; plan 05 adds it. Do not invent the enum.

### 0.2 `MissingSlot` already exists — do not redeclare it, extend it

Plan 02 §2 declares `MissingSlot` as new in `router/mod.rs`. Plan 03 already delivered it at
`nl2sql/ir/spec.rs:364`, with **three** of the five variants:

```rust
pub enum MissingSlot { Subject, Dimension, Metric }   // delivered
```

A second enum of the same name in `router/` is a defect: `ParseOutcome::Parsed { spec, missing:
Vec<MissingSlot> }` (`ir/parse.rs`) already returns the IR's variant, and Tier 1.5 consumes it
directly.

- **Re-export**, do not redefine: `pub use crate::nl2sql::ir::MissingSlot;` in `router/mod.rs`.
- **Add** `Patient` and `TimeRange` to the enum in `ir/spec.rs`. Without them, two of §5's five
  clarify templates are unreachable. Adding variants is additive; fix any non-exhaustive `match`
  the compiler flags in `ir/`.

### 0.3 `BindingCoverage` has no per-line map

§3.5 step 1 reads `binding.coverage.lines[l].usable`. What plan 01 delivered
(`ontology/binding.rs:75`) is:

```rust
pub struct BindingCoverage { pub total_tables: usize, pub bound_tables: usize,
    pub orphan_tables: usize, pub exact_concepts: usize, pub usable_lines: Vec<ServiceLine> }
```

No `lines` map. Use the existing predicate instead — `line_is_usable` (`binding.rs:115`) is
currently **private**; make it `pub`. Prefer it over `coverage()`, which walks every table for
every concept and is wasteful per-request.

### 0.4 `tables_for(line)` does not exist — add it, and note the shared-concept asymmetry

§3.5 step 2 needs `scope = binding.tables_for(line)`. Plan 01 delivered only
`tables_for_concept(EntityConcept)` (`binding.rs:140`). Add one accessor to `impl SchemaBinding`:

```rust
pub fn tables_for_line(&self, line: ServiceLine) -> Vec<&TableBinding>
```

**This is the trap in this plan, so read it twice.** `line_is_usable` deliberately *excludes*
`SHARED_CONCEPTS` (Patient, Encounter, Provider, Department, DiagnosisCode) — a line is not usable
on shared concepts alone. `tables_for_line` must do the **opposite** and *include* them: scope is a
read allow-list, and almost every service-line query joins out to Patient or Encounter. Excluding
them makes every such join fail `validate_sql`'s allow-list check.

Two different questions over the same ownership list:
*usability* = "does this line have its own subject matter?" (shared **excluded**);
*scope* = "what may this line read?" (shared **included**).
Put exactly that in the doc comment on both functions.

Reminder from plan 03's review: scope is a read allow-list, **not** a security boundary. RBAC stays
in `auth/`.

### 0.5 `extract_record_identifier` moved in plan 03

§3.2 says "reuse `nl2sql::routes::extract_record_identifier`". Plan 03's §5 migration moved it to
`nl2sql/text.rs:19`, `pub(crate)`. Same for `contains_likely_person_name` and `normalize_question`.
Use `crate::nl2sql::text::*`.

### 0.6 `RouteEntities` is the Tier-2 *model* deserialization target — split it

Today (`router/mod.rs:161`) it is `#[derive(Debug, Clone, Default, Deserialize)]` with three
string-typed fields, and it is populated by `serde` from the `classify_route` tool call
(`RouteToolOutput.entities`). §2 redefines it with typed fields — `concepts: Vec<EntityConcept>`,
`time_range: Option<TimeRange>`, `metric: Option<MetricHint>`.

That collides: one struct cannot be both a small local model's JSON output shape and the internal
typed carrier. `metric` changing from `Option<String>` to `Option<MetricHint>` silently breaks
Tier-2 deserialization for any value the model phrases differently — the model's output stops
parsing and the tier degrades to nothing, quietly.

**Resolution — the same split plan 03 used for `PlannedSpec` vs `QuerySpec` (03a §0.4):**

- Keep a flat, string-typed, `Deserialize` wire struct as the model's output target. Rename the
  existing three-field struct to `RouteEntitiesWire` and leave its shape alone.
- `RouteEntities` becomes the internal typed struct from §2, deriving **`Serialize + Deserialize +
  Default + Clone + Debug`**. `Serialize` is required: it becomes a field of `RouteDecision`, which
  `to_sse_json` (`router/mod.rs:131`) emits.
- One conversion `fn from_wire(wire: RouteEntitiesWire, binding: &SchemaBinding) -> RouteEntities`,
  resolving concept and metric strings against the binding. **Unrecognised values are dropped, never
  guessed** — a hallucinated concept must not become a subject.

Also flip `RouteDecision.entities` from `Option<RouteEntities>` (`mod.rs:104`) to always-present
`RouteEntities` per §2. It is `#[allow(dead_code)]` and unconsumed today, so this is low-risk — drop
the `allow` once v3 reads it.

### 0.7 `TimeRange` already exists — as an *enum*, not the struct plan 02 assumes

§2 and §3.2 describe `TimeRange { start, end, grain }`. `nl2sql/ir/spec.rs:203` already defines a
`TimeRange` **enum** (`Absolute { lo, hi }`, plus further variants — read the file), alongside
`TimeScope { column, range, bucket }` at `:192` and `BucketUnit`.

Tier 1.5 passes entities straight into `ir::parse`, so this is precisely where a duplicate type
would collide. **Reuse the IR's `TimeRange`**; do not define a second one in `router/`. Read the
full enum before writing `router/time.rs`, and have `time.rs` return the IR's type directly.

`MetricHint` genuinely does not exist — define it in `router/`, but map it explicitly onto the
existing `QueryIntent` (`aggregation/intent.rs:16`) and the IR's measure/aggregate types. A metric
the IR cannot express must not be representable as a "successful" hint.

**Injectable now, per 03a §0.3:** `router/time.rs` must take `now: DateTime<Utc>` as a parameter.
Never read the clock inside it, or §8's 30-phrase suite starts failing tomorrow.

### 0.8 `ONPREM_ROUTER_V3` does not exist, and plan 02 §6 omits it

Plan 08 §4 step 3 sequences this plan behind `ONPREM_ROUTER_V3=false`. Plan 02 §6 lists only
`ONPREM_ROUTER_DETERMINISTIC_FIRST`, `ONPREM_ROUTER_CLARIFY_ENABLED` and
`ONPREM_ROUTER_MODEL_MIN_CONFIDENCE`. Plan 08 wins: add `ONPREM_ROUTER_V3` (**false**) as the master
switch; §6's three keys are sub-switches that only take effect when v3 is on.

Existing keys to reuse verbatim: `ONPREM_ROUTER_MODEL_ENABLED`, `ONPREM_ROUTER_CACHE_SIZE`. Use the
existing `env_or` / `env_parse` helpers. Add
all four new keys to the **root** `.env.example` — never create `onprem-rag-server/.env`.

### 0.9 One stale comment to fix

`router/mod.rs:68-72` says the backend distinction "will be reintroduced when plan 18 Phase D
(text-to-SQL backend selection) actually lands". `StructuredBackend` is present (`mod.rs:86`) and
read at the `rag/routes.rs` and `agents/routes.rs` call sites. §3.5 *is* that work. Update the
comment; do not leave a note claiming the feature is absent.

---

## 1. Files

```
onprem-rag-server/src/router/
  mod.rs             // types, route_v3(), cache key; keep route() as the v2 control arm
  focus.rs           // new — minimal ConversationFocus (§0.1) + resolve()
  entities.rs        // new — binding-driven lexicon, service-line argmax
  time.rs            // new — phrase -> ir::TimeRange, injected `now`
  backend.rs         // new — select_backend()
  clarify.rs         // new — §5 templates, no model call
  conversational.rs  // unchanged
```

Also touched: `ontology/binding.rs` (§0.3 visibility + §0.4 accessor), `nl2sql/ir/spec.rs` (§0.2 two
variants), `aggregation/intent.rs` (metric mapping only), `config.rs`, `rag/routes.rs`,
`agents/routes.rs`, root `.env.example`.

Tests are inline `#[cfg(test)] mod tests`.

## 2. Order of work

Bottom-up, so each step is independently testable and the PR stays reviewable:

1. `ontology/binding.rs` — `pub line_is_usable`, new `tables_for_line`. Test the §0.4 asymmetry
   directly.
2. `ir/spec.rs` — add the two `MissingSlot` variants.
3. `router/time.rs` — 30 phrases against a frozen `now`. Pure; needs no other module.
4. `router/entities.rs` — lexicon built from the binding, then service-line argmax.
5. `router/focus.rs` — the minimal type plus §3.1's deterministic substitutions.
6. `router/backend.rs` — §3.5's four-step selection.
7. `router/clarify.rs` — the five templates.
8. `router/mod.rs` — `route_v3()` assembling tiers 0 → 1 → 1.5 → 2 → 3, then flag dispatch.

## 3. Non-negotiables

- **PHI stays on-prem.** No new network destination. Routing is model-free except the existing
  Tier-2 `classify_route` call through `FoundryManager`.
- **Routing decides; it never executes.** No SQL runs in `router/`. Everything Tier 1.5 produces
  still goes through `nl2sql::validate::validate_sql` on the execution path — no exceptions.
- **No PII in the lexicon.** §3.2 builds enum terms from `ColumnBinding.enum_values`; plan 01
  guarantees no PII column contributes `enum_values`, and that invariant must not be worked around
  here. Never build a lexicon term from a column with `is_pii == true`.
- Scope is a read allow-list, not a security boundary (§0.4).
- **v2 `route()` keeps working unchanged** when the flag is off — it is the control arm that
  justifies flipping. No deletions in this PR.
- New response fields (`service_line`, `deterministic`, `scope_size`, `source_id`, clarify
  `question`/`slot`) must be mirrored in `src-tauri/src/commands.rs` **and** `src/lib/bridge.ts`, or
  they are silently dropped. §2 says the `routed` contract is *extended*, not changed — verify an old
  client still parses it.
- JSON-encode any SSE token payload (a bare `data:` strips leading spaces and fuses words).

## 4. Wiring behind the flag

Four call sites take the v2/v3 branch — verified line numbers:

| File | Line | Call |
|---|---|---|
| `rag/routes.rs` | 88 | `crate::router::route(` |
| `rag/routes.rs` | 301 | `crate::router::route(` |
| `agents/routes.rs` | 140 | `crate::router::route(` |
| `agents/routes.rs` | 356 | `crate::router::route(` |

v2 signature to preserve (`router/mod.rs:288`):
`route(question, has_history, config, foundry_opt, classify_spec, cache) -> RouteDecision`.

Add alongside it:

```rust
pub async fn route_v3(state: &AppState, req: RouteRequest<'_>) -> RouteDecision

pub struct RouteRequest<'a> {
    pub question: &'a str,
    pub memory: &'a WorkingMemory,        // memory.rs
    pub focus: &'a ConversationFocus,     // §0.1, plan-02-owned
    pub fixed_line: Option<ServiceLine>,
    pub trace: &'a RequestTrace,          // telemetry.rs
}
```

Tier 1.5 calls `nl2sql::ir::parse(&resolved, Some(&entities), binding, scope)` — the delivered
signature is `hint: Option<&RouteEntities>`, so pass `Some(...)`, and that `RouteEntities` is the
**typed** struct from §0.6.

Cache key must include the binding identity so a rebuild invalidates it — use
`SchemaBinding.override_version` plus `bound_at`, both already on the struct.

## 5. Tests

Plan 02 §8 in full, plus:

- §0.4's asymmetry: one binding where a line is **not** usable yet its scope is **non-empty**
  (shared tables only). Both assertions in one test so the distinction cannot silently collapse.
- `MissingSlot::Patient` and `::TimeRange` each reach their §5 template.
- **Flag off ⇒ `route_v3` is never called and v2 decisions are identical to today.** This is the
  no-regression arm; assert it explicitly.
- Tier-2 wire → typed conversion (§0.6) drops unrecognised concept and metric strings rather than
  guessing.
- §8's clarify case in both directions: *"how many were cancelled?"* with `ConversationFocus::default()`
  → `Clarify(Subject)`; with `focus.concept = Some(Appointment)` → `Structured`.
- Entity extraction against **both** plan-01 fixtures (`dev_seed_binding.json`,
  `alt_schema_binding.json`). §8's example is dev-only; the alt binding must yield the same
  `ServiceLine` and `MissingSlot` outcomes under different physical names. A rule that only works on
  dev is a defect, not partial success — and where the alt schema genuinely lacks the concept, that
  absence is the expected outcome and must be asserted as such, not smoothed over.

## 6. Acceptance / gates

```
cd onprem-rag-server && cargo check && cargo test --bin onprem-server
```

- Router accuracy **≥ 0.95** on the extended `eval/data/router.jsonl` (the file exists — extend it,
  do not replace it).
- Median routing latency **< 50 ms** with Tier 2 not invoked, and Tier 2 invoked on **≤ 25 %** of the
  fixture. Both are measurable in-process with no live stack and no model call — **measure them and
  report the numbers.** An unmeasured gate is an unmet gate.
- No conversation ever emits two consecutive `clarify` events.
- A-series questions assert `deterministic == true` (they must never reach Tier 2).
- **Do not run `cargo run`** — os error 4551 on this host (Smart App Control). `cargo check` /
  `cargo test --bin onprem-server` are the verification.

## 7. Reporting contract

- **Never** lower a threshold, weaken a fixture, or relax an assertion to make a test pass.
  **A number below the bar is an ACCEPTED deliverable. A number at the bar obtained by editing the
  fixture or the question is a REJECTED deliverable.** Report shortfalls with a per-cause diagnosis.
- If a plan-02 requirement cannot be met without a type from plan 05 or 06, say so and stop — do not
  invent those types beyond §0.1's minimal `ConversationFocus`.
- Report every deviation from this brief explicitly, including ones you consider improvements.

## 8. Out of scope

- `StructuredExecutor`, `RetrievalFilter`, provenance SSE — plan 04.
- `AgentKind::Line`, personas, `AgentMode`, linker scope enforcement — plan 05.
- Persisting/populating `ConversationFocus`, spec mutation, suggestions — plan 06.
- Bridge and TypeScript beyond mirroring the `routed`/`clarify` fields — plan 07.
- Deleting v2 `route()` — a later PR, once accuracy holds on the flag-on path.
