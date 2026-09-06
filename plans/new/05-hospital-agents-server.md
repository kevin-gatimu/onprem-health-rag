<!-- markdownlint-disable MD013 -->

# 05 — Hospital agents (server)

**Goal:** replace the mechanism-named agents (`health_query`, `trends`, `patient_lookup`,
`summarize`) with the hospital service-line roster, keep **Ask** as the router-driven entry point,
implement **Service Trends** and **Handover** as modes, give every agent a persona *generated from
the schema binding* (so it speaks the hospital's own vocabulary), and enforce table ownership at
the four hooks listed in the data-map §6.

**Depends on:** 01, 04. **Unblocks:** 07.

---

## 1. Identity model

Two orthogonal enums replace the single overloaded `AgentKind`:

```rust
// foundry/router.rs — model roles only (what a model call is for). Rename for clarity.
pub enum ModelRole { Grounded, Narrate, Rewrite, Classify, Extract, Verify, TextToSql, PlanSpec, Compact }

// agents/kind.rs — product identity (what the user talks to)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind { Ask, Line(ServiceLine) }        // serialises as "ask" | "<service_line slug>"

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode { #[default] Ask, Trends, Handover }
```

`ModelSpec::for_role(role, cfg)` replaces `for_kind`; the per-kind temperature/thinking table
collapses to per-role (Grounded 0.3, Narrate 0.2, Rewrite 0.1, Classify 0.0, TextToSql 0.0,
PlanSpec 0.0, Extract 0.1, Verify 0.1, Compact 0.2). `Trends` mode enables `thinking` on
`Narrate` as today's `Trends` kind did. Persisted role overrides (`settings.rs`) keep the single
`chat` override key.

**Legacy compatibility (one release):** `/agents/health_query|trends|patient_lookup|summarize|chat`
map to `Ask` with mode `Ask|Trends|Ask|Handover|Ask`; `agent_kind` on old conversations is read
through the same mapping; the UI stops sending them after plan 07.

## 2. Endpoint

`POST /agents/<kind>` body:

```rust
pub struct AgentRequest {
    pub question: String,
    pub conversation_id: Option<String>,
    pub run_id: Option<String>,
    #[serde(default)] pub mode: AgentMode,          // Trends / Handover toggles from the UI
    pub source_id: Option<String>,                  // optional pin when several sources are bound
}
```

Flow: load memory + focus → `route_v3(fixed_line = kind.line(), mode)` → `executor::run` →
narrate with persona → suggestions (06) → persist (`agent_kind` = slug, `mode`). **Ask** is the
same flow with `fixed_line = None`; the decision's `service_line` is emitted in `routed` so the
UI can badge which department answered, and — when the decision is `Semantic` with no line — Ask
answers from the whole corpus as today.

`GET /agents` (plan 01 §8) is the registry the UI renders; it includes per-agent `example_questions`
(§4) and `modes: ["ask","trends","handover"]`.

## 3. Ownership enforcement — the four hooks

| Hook | Change |
|---|---|
| Linker `nl2sql/linker.rs::link` | new arg `scope: Option<&[String]>`; candidate cards filtered to scope **before** scoring; explicit-mention boost still applies but cannot escape scope (a Maternity question naming `payments` gets a "that's outside this agent — try Revenue or Ask" clarify, not a join). |
| Aggregation `aggregation/catalog.rs` | `Catalog::scoped(scope)` used for planner prompt and validation (plan 04 §3). |
| Retrieval `retrieval/mod.rs` | `RetrievalFilter { tables: scope, explicit: kind != Ask }` (plan 04 §4). |
| Identity | `AgentKind::Line(l)` ⇒ scope = `binding.tables_for(l)` ∪ shared concept tables; `Ask` ⇒ scope from router decision or none. |

Shared concepts (`Patient, Encounter, Provider, Department, DiagnosisCode`) are always in scope
(data-map §6 rule 2). **Patient Chart** additionally admits any table with `patient_path.len() ≤ 2`
*when the question carries a patient key*, so "what is PT-00042 allergic to / owed / booked for"
works from the chart without hopping tabs.

Test (data-map §6): for the dev binding, every table in `schema_catalog` is in the scope of at
least one `ServiceLine` — the "no orphans" assertion — run as `cargo test` against the fixture in
plan 01 §7 and live as an admin diagnostic in `GET /sources/<id>/binding` (`orphans: []`).

## 4. Personas generated from the binding (`agents/persona.rs`)

```rust
pub fn system_prompt(kind: AgentKind, mode: AgentMode, binding: Option<&SchemaBinding>, focus: &ConversationFocus) -> String
```

Composed from fixed blocks + binding facts, ≤ ~350 tokens:

1. **Role line** (fixed per line): "You are the Maternity assistant for this hospital's records system: antenatal care through delivery to the newborn."
2. **Data you can see**: bound concept labels with the *hospital's* table names and the enum vocab: "Deliveries (`deliveries`): delivery_mode ∈ spontaneous_vaginal, caesarean, vacuum_assisted…; outcome ∈ live_birth, stillbirth…". Enum lists capped at 8 values each; PII columns never listed.
3. **Grounding rules** (fixed): narrate only supplied rows/passages; no clinical advice; say when scope excludes something and name the agent that owns it (`ServiceLine::owner_of(concept)`).
4. **Mode block**: `Trends` — "Describe direction, magnitude and the bucket with the largest change; never extrapolate"; `Handover` — SBAR headings, ≤ 8 bullets, flag anything critical/abnormal present in the rows.
5. **Focus block** (06): "Current focus: patient PT-00042 (Jane Chebet); period: August 2026."

`ServiceLine::examples()` returns the data-map §5 question list per line, each tagged with the
concepts it requires; `GET /agents` returns only those whose concepts are bound for some source.

## 5. Capability questions

Tier 0 gains a `Capability` route ("what can you do", "what data do you have", "help") answered
without a model from `persona::capability_answer(kind, binding)` — the concept list and three
example questions — so the answer is truthful and bounded per tab (data-map §1 consequence 3).

## 6. Modes

- **Trends**: router forces `Shape::Trend` when the IR parse yields `Scalar`/`Grouped` with a time
  range or when no time bucket was stated (default bucket: month; range: last 12 months if none).
  Narration in Trends mode receives rows sorted chronologically and the persona Trends block. Chart
  spec for the UI unchanged (`specToChart`).
- **Handover**: forces `Hybrid` when a cohort is stated ("handover for Medical Ward B"), else
  `Semantic` with `RetrievalFilter` on the line's tables and the focus patient/ward; time defaults
  to the last 24 h on `EventTime`. Output is a synthesis; citations required.

## 7. Conversations

`routes/conversations.rs`: `agent_kind` stores the slug; `GET /agent-conversations?kind=` accepts
new slugs and legacy names; new optional `mode` on messages. `title` auto-generation (existing)
prefixes the line label for Ask answers routed to a line ("Maternity · Deliveries last month").

## 8. Files

- `agents/kind.rs`, `agents/persona.rs`, `agents/registry.rs` (`GET /agents`), `agents/routes.rs` (rewrite to the shared flow — expected to shrink from 1121 lines to ~300).
- `foundry/router.rs` (`ModelRole`), `state.rs::spec_for(role)`, `settings.rs` (override keys), `config.rs` (`ONPREM_MODEL_*` keys keep names; add `ONPREM_MODEL_PLAN_SPEC` defaulting to the SQL model).
- `router/conversational.rs` (`Capability`).
- `nl2sql/linker.rs` (`scope`).
- Delete: `agents/routes.rs::{should_try_deterministic_sql, class_to_agent_kind}`, per-kind branches.

## 9. Tests

- Scope: Maternity agent asking "how much did we collect by M-Pesa" → `Clarify`/redirect to Revenue, never a `payments` query.
- Persona builder: dev binding produces a prompt naming `deliveries`, `newborns`, `antenatal_visits` and no PII column; alt binding names `OB_Delivery` etc.
- Legacy mapping table.
- Capability answer on each of the 13 lines returns ≥ 1 concept and ≥ 1 example on the dev seed.
- Live: `POST /agents/maternity` "how many deliveries last month, and how many were caesarean" → `SourceSql`, two rows or one row two columns, `provenance.path == [Link(Hit), DeterministicSql(Hit), Validate(Hit), Execute(Hit)]`.

## 10. Acceptance

- All 13 lines + Ask reachable; Tier-1 six (Ask, Patient Chart, Front Desk, Ward Board, Pharmacy, Diagnostics) pass 10 questions each from data-map §5 on the dev seed via `SourceSql` or `DocDb` (not semantic) — tracked in plan 08 T-series.
- Every agent answers "what can you do?" without a model call in < 100 ms.
