<!-- markdownlint-disable MD013 -->

# 06 — Conversation memory, focus and suggestions

**Goal:** make conversations fluid. The assistant must (a) know *what* is being talked about —
patient, ward, period, last result — not just *what was said*; (b) resolve follow-ups cheaply and
deterministically; (c) answer slot-filling replies after a clarify; (d) offer 2–4 grounded,
answerable next questions after each turn; (e) keep the existing rolling summary for long-range
context.

**Depends on:** 02 (router consumes focus), 04 (executor emits outcome). **Unblocks:** 07.

---

## 1. Two memories, distinct jobs

| | `WorkingMemory` (exists) | `ConversationFocus` (new) |
|---|---|---|
| Content | rolling summary + verbatim tail of turns | typed slots: entities, time range, last `QuerySpec`, last result shape, active agent/line, pending clarify |
| Producer | persistence + write-behind compaction | executor outcome after each turn (no model) |
| Consumer | model prompts (grounded, rewrite, meta) | router focus resolution, persona focus block, suggestions, IR follow-up mutation |
| Cost | model call on compaction | zero model calls |

Focus is authoritative for *reference*; the summary is context for *style and long-range recall*.
Conversation-meta questions ("what have I asked?") keep using `WorkingMemory`.

## 2. `ConversationFocus` (`memory/focus.rs`)

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversationFocus {
    pub patient: Option<FocusEntity>,        // { key: "PT-00042", display: "Jane Chebet", row_pk, table, set_turn }
    pub provider: Option<FocusEntity>,
    pub place: Option<FocusEntity>,          // ward / department / theatre / store
    pub concept: Option<EntityConcept>,      // last subject
    pub service_line: Option<ServiceLine>,
    pub time_range: Option<TimeRange>,
    pub last_spec: Option<QuerySpec>,        // plan 03 IR of the last structured answer
    pub last_result: Option<ResultDigest>,   // { shape, row_count, columns, top_labels: Vec<String> ≤ 5, scalar: Option<f64> }
    pub pending_clarify: Option<MissingSlot>,
    pub turn: u32,
    pub updated_at: DateTime,
}
```

Stored on the `conversations` document as `focus` (BSON), loaded with `load_working_memory` in
one round-trip (`load_context(db, cid, config) -> (WorkingMemory, ConversationFocus)`), written
by the executor's persistence step. Stateless requests (no `conversation_id`) get `Default`.

### 2.1 Update rules (`focus::apply(outcome, decision, question) -> ConversationFocus`)

- `patient`: set when a decision's `entities.patient_key` resolves, or when a `SourceSql`/`List`
  result has exactly one distinct `BusinessId`-role value on the Patient table (single-patient
  answer), or when a `Lookup` returned one row. Cleared when a new patient key appears; **kept**
  across aggregate questions (asking "how many beds are free" does not forget the patient).
- `place`: from `enum_filters`/joins on `WardRef`/`DepartmentRef`/`Location`.
- `time_range`: from `entities.time_range` when explicit; kept otherwise.
- `concept`/`last_spec`/`last_result`: from `ExecOutcome`. `top_labels` come from `AggRow.label`
  or the first column of a `SourceSql` grouped result (≤ 5, truncated to 40 chars, non-PII roles
  only — never a person name column unless the question was about that person).
- `service_line`: from decision; on dedicated tabs it is fixed.
- `pending_clarify`: set on `Clarify`, cleared after any answer.
- Reset entirely on an explicit "new topic"/"forget that"/"start over" (Tier 0 regex).

## 3. Focus resolution (`router/focus_resolve.rs`, invoked by plan 02 §3.1)

Model-free rewrite producing `Resolved { question, substitutions: Vec<Substitution> }`:

| Pattern | Rewrite |
|---|---|
| `he|she|they|him|her|his|their|the patient|that patient|this patient` | → `patient {key}` (only if `focus.patient` is Some) |
| `there|that ward|the same ward|that department` | → the `place.display` |
| `then|that period|same period|the same month|those dates` | → `time_range` rendered as "between {start} and {end}" |
| `that doctor|the same provider|him/her` when `provider` is Some and `patient` is None | → provider display |
| Bare ellipsis: `and by gender?`, `what about last month?`, `only caesareans`, `just ICU`, `as a percentage`, `per week instead`, `top 5` | → **spec mutation** (§3.1) — no textual rewrite |
| `why?` / `explain that` / `summarise those` after a structured result | → `Hybrid` with `last_spec` as cohort |
| Slot answer while `pending_clarify` is Some (e.g. "appointments", "last quarter", "PT-00042") | → merge into the pending `last_spec`/question and re-route (§4) |

Every substitution is recorded in `RouteEntities` with `source: Focus`, emitted in `routed`
(`"focus_used": ["patient","time_range"]`) so the UI can show a subtle "using: Jane Chebet · Aug
2026" chip, and persisted with the message.

### 3.1 Spec mutation (`nl2sql/ir/mutate.rs`)

```rust
pub fn mutate(last: &QuerySpec, phrase: &str, ents: &RouteEntities, binding: &SchemaBinding) -> Option<QuerySpec>
```

Rules: `by|per <dim>` → replace/add `Dimension`; `what about|and for <time>` → replace `TimeScope.range`;
`only|just <enum value>` → add `Filter`; `as a percentage|rate` → `Shape::Rate` with the last filter
as numerator; `per <unit> instead` → `bucket`; `top <n>` → `limit` + order desc; `the other way|ascending`
→ flip order; `without <filter phrase>` → remove matching filter; `same for <concept>` → swap subject
when the roles still bind. The result is a normal `QuerySpec` — router reports
`Structured{SourceSql}`, `deterministic: true`, `provenance.spec.provenance.derived_from = Some(prev_message_id)`.

## 4. Clarify round-trip

When `focus.pending_clarify == Some(slot)` and the new question is short (≤ 6 tokens) or matches
the slot type (identifier, time phrase, concept term), the resolver fills the slot in
`focus.last_spec` (or the stored partial spec on the clarify message) and re-runs the router with
the merged spec. If the fill fails, treat the text as a fresh question and clear
`pending_clarify` (never clarify twice — plan 02 §5).

## 5. Suggestions (`answer/suggest.rs`)

After each answer, produce ≤ 4 **answerable** follow-ups without a model, ranked, and emit them as
SSE `suggestions` `[{ text, kind: "drill|widen|compare|switch|explain", spec?: QuerySpec, agent?: slug }]`.
Each suggestion is pre-bound: it is generated *from* a mutated `QuerySpec` (§3.1) that already
passes `bind`, so clicking it never produces a clarify or a miss. Generation rules by outcome:

| Outcome | Candidates |
|---|---|
| Scalar count | `by <best Dimension>` (Status/Category enum with 2–8 values, or `*Ref` with a name column), `trend per month`, `only <most common enum value>` (from `last_result.top_labels`), `as a rate of <parent concept>` |
| Grouped | `drill into <top label>` (adds filter), `switch dimension to <next best>`, `trend of <top label> per month` |
| Trend | `compare with previous period`, `by <dimension> for the peak bucket`, `list the <subject> in <peak bucket>` |
| List/Lookup on a patient | patient-chart set: `allergies`, `active prescriptions`, `last vitals`, `open bills`, `upcoming appointments` — only those whose concepts are bound and have a `patient_path` |
| Semantic | `show the exact numbers` if the passages' table has an `EventTime` (→ Scalar count on that concept with focus time), `who is the owning agent` switch when the line ≠ current tab |
| Clarify | the clarify `options` themselves |
| Empty result | `widen`: drop the last filter / extend time range ×4 / remove dimension |

Ranking: prefer suggestions that use a *different* mutation kind from the last turn, that stay
within the current agent's scope, and whose enum cardinality is 2–8. Persist with the assistant
message (`suggestions` field) so reloads show them; the UI (plan 07) sends a click as a normal
question **plus** `suggestion_spec` so the router can skip parsing (`RouteRequest.preparsed`).

Model-generated suggestions are explicitly **not** used: they cannot be guaranteed answerable and
cost a generation. If later wanted, add `ONPREM_SUGGEST_MODEL=true` behind the same interface.

## 6. Prompt use of focus and memory

- Grounded/semantic prompt: `focus block` (persona §4 item 5) + `WorkingMemory.summary` + tail (existing).
- Narration: `focus block` only (rows are the evidence).
- Rewrite role (`QueryRewrite`): receives the *resolved* question, so the model never has to resolve pronouns — cheaper and deterministic.
- `ConversationMeta`: unchanged.

Compaction (`maybe_spawn_compaction`) additionally passes `focus` to the summariser so the summary
mentions the focus patient/period explicitly (still PHI on-prem; unchanged handling).

## 7. Config

`ONPREM_FOCUS_ENABLED` (true), `ONPREM_SUGGESTIONS_MAX` (4), `ONPREM_FOCUS_PATIENT_TTL_TURNS` (0 = keep until replaced), `ONPREM_FOCUS_TOP_LABELS` (5).

## 8. Files

- `memory/mod.rs` (moved from `memory.rs`), `memory/focus.rs`, `router/focus_resolve.rs`, `nl2sql/ir/mutate.rs`, `answer/suggest.rs` — new.
- `routes/conversations.rs` — `focus` on conversation doc; `GET /conversations/<id>/messages` returns `suggestions`, `focus_used`, `provenance`, `spec`.
- `rag/routes.rs`, `agents/routes.rs` — load `(memory, focus)`, pass to router/executor, persist updated focus; emit `suggestions` before `done`.
- Delete `rag/routes.rs::{resolve_followup_sql, resolve_semantic_sql}` (superseded).

## 9. Tests

- Focus update: A10a → A10b (`what is the patient's name?`) resolves via focus without a model or a `resolve_followup_sql` special case.
- Sequence on dev seed: "how many admissions last month" → "by ward" → "only ICU" → "as a percentage" → "what about the month before" → "why?" — assert each decision `deterministic` (first five) then `Hybrid`, and the specs form the expected chain.
- Clarify round-trip: "how many were cancelled?" → clarify(Subject) → "appointments" → Structured result.
- Suggestions: every emitted suggestion's `spec` passes `bind` on the dev binding; none reference PII roles; count ≤ 4.
- Reset: "new topic" clears focus.

## 10. Acceptance

- Zero model calls for pronoun/ellipsis resolution in the fixture sequences.
- ≥ 90 % of suggestion clicks on the dev seed answer deterministically (`provenance.path` contains `DeterministicSql(Hit)` or `Aggregation(Hit)`).
- A10b latency stays < 400 ms.
