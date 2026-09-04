# Ingestion extractor + faithfulness verifier (Phase 4, part 1)

> Implements the two roles that shipped as scaffolding in Phase 1 and had sat at
> `status: "planned"` on the Models page ever since, both labelled "(Wired in Phase 4.)":
> `AgentKind::Extract` and `AgentKind::Verify`. Shipped 2026-08-29.
>
> Companion docs: `plans/docs/model-selection-and-routing.md` (role table),
> `plans/06-agents-routing-and-aggregation.md` (what Phase 4 still owes).

## Scope

This note covers **only** the two model roles. The rest of the Phase 4 list in
`06-agents-routing-and-aggregation.md` — on-demand `mistral-nemo-12b` summarizer, the
agentic multi-hop loop, ingest-time catalog population, and the cohort semantic
pre-filter — is untouched and still open.

**PHI decision: annotate, never scrub.** The role table in
`model-selection-and-routing.md` describes the extractor as "flag/scrub PHI", but the
Models-page card promises only "pulls structured fields ... maps to ICD-10/LOINC". We
built the card's version. The embedded `text` and `contentVector` are byte-identical
with the extractor on or off, so retrieval behaviour is unchanged and disabling the
feature is a config flip rather than a re-ingest. Redaction would have meant a false
positive silently destroying clinical content with no way back short of re-ingesting
the source — not a default worth shipping quietly.

## Ingestion extractor (`AgentKind::Extract`)

`onprem-rag-server/src/ingest/extract.rs` (new).

Reads a row's free-text projection and returns the clinical entities it names, each
mapped to a standard vocabulary. Stored as `records.extracted`:

```json
{
  "conditions":  [{ "text": "type 2 diabetes", "code": "E11",    "system": "icd10"  }],
  "medications": [{ "text": "metformin 500mg", "code": "860975", "system": "rxnorm" }],
  "labs":        [{ "text": "HbA1c 7.2%",      "code": "4548-4", "system": "loinc"  }],
  "model": "phi-4-mini"
}
```

Design points worth remembering:

- **The model never chooses a code system.** It is asked for `{text, code}` only; the
  vocabulary is a property of the array it arrived in and is stamped on afterwards by
  `stamp_systems`, which also overwrites any `system` the model volunteered. One whole
  class of mistake removed by schema shape rather than by prompt.
- **An empty code is a correct answer.** Rule (6) of the system prompt tells the model
  to leave `code` blank rather than guess; small models invent plausible ICD-10 codes
  with great confidence. A term with a blank code still survives — it is a search aid.
- **Codes are not validated.** There is no terminology server on-prem. Treat them as
  retrieval hints, not as billing-grade coding.
- **Extraction runs on the whole row, before chunking**, so an entity split across a
  chunk boundary is still seen once and whole. The annotation is then cloned onto every
  chunk of that row, exactly as `fields` already is.
- **It cannot fail an ingest.** Timeout, missing model, unparseable JSON — all return
  `None` and the row is stored unannotated. `extract_batch` only ever returns what it
  managed to annotate.
- **Rows are taken by value, not by slice.** A future borrowing from the caller's
  buffer is generic over that borrow's lifetime, which makes the whole
  `buffer_unordered` stream higher-ranked and costs `ingest::run` the `Send` bound
  `tokio::spawn` needs. This cost an hour; do not "optimise" it back to a slice.

Eligibility is a word-count floor (`ONPREM_EXTRACT_MIN_WORDS`, default 40): below it a
row is a structured column dump, and the extractor would spend an NPU round-trip to
find nothing.

Progress: `jobs.extracted_rows` is a new counter, surfaced through `IngestProgress` →
bridge → an "Annotated" stat that appears in the ingest UI only when non-zero.

## Faithfulness verifier (`AgentKind::Verify`)

`onprem-rag-server/src/verify.rs` (new), hooked into the `/chat` semantic branch.

Breaks a finished answer into its clinical claims and checks each against the passages
that were retrieved to support it.

- **Runs after the answer has fully streamed.** The user already has every token; the
  check delays the verdict badge and nothing else. That is why it is a post-stream step
  rather than a gate in front of generation.
- **Fails open, visibly.** Any failure produces `status: "skipped"` with a `reason`.
  Suppressing an answer because a 3.8B model had a bad day would be worse than not
  checking.
- **An empty claim list is `skipped`, not `supported`.** A verifier that found nothing
  to check has not cleared the answer, and rendering that as a pass is precisely the
  false assurance this module exists to prevent.
- **A "supported" claim citing no real passage is demoted to unsupported.** Passage
  refs are clamped to the evidence actually supplied — small models cite passage 9 of 4.

The pre-existing deterministic citation-marker check (`[N]` pointing past the end of
the citation list) folded into the same report as `citation_overflow`, so `/chat` emits
exactly one `verify` event. Its old shape (`{status:"citation_overflow", invalid:[…]}`)
was never consumed — the bridge dropped the event entirely — so nothing broke.

## Two bugs found and fixed on the way

Both pre-existing, both surfaced by running the app during this work.

**1. Forced tool call + JSON-schema response format is an unsatisfiable grammar.**
`plan_tool` set `tool_choice: Function(...)` *and*
`response_format: JsonSchema(<the argument schema>)`. Those are two grammars over the
same output, and ORT-GenAI intersects rather than layers them: the output must satisfy
the tool-call envelope `{name, arguments}` **and** the bare argument object at once.
Result:

```
Error creating grammar: Unsatisfiable schema: required item is unsatisfiable
```

Observed on the NPU phi-4-mini `classify` call (the router logged "Tier 2 classify
failed; falling open" and silently degraded to lexical routing) while the GPU qwen path
tolerated the pair. `plan_tool` now retries with the forced tool call alone when
`is_grammar_error` matches — `try_parse_tool` already accepts a plain JSON content
response, so the schema is the expendable half. This blocked the new extractor and
verifier tools too, which go through the same helper.

**2. `generate_stream_with` offered a tool nobody executes.** Every caller streams the
model's *content* straight to the user — narration of already-executed rows, or
grounded semantic generation. None of them parse or execute a tool call. It offered
`run_aggregation` anyway whenever `spec.tools` was set, and `PatientLookup` sets it, so
"list 5 patients" produced a literal

```
<tool_call> {"name": "run_aggregation", "arguments": {...}} </tool_call>
```

block in the chat transcript — content, and `ThinkFilter` only strips `<think>`. Tools
now belong exclusively to `plan_tool`, which forces, parses, validates, and executes
them. `spec.tools` still gates that path.

## Configuration

Both features are **off by default**. Each costs a model round-trip on a hot path
(per ingested row; per answer) and each needs its phi-4-mini model downloaded, so
opt-in is the honest default.

```
ONPREM_EXTRACT_ENABLED=false      # master switch
ONPREM_EXTRACT_MIN_WORDS=40       # eligibility floor
ONPREM_EXTRACT_CONCURRENCY=2      # rows in flight
ONPREM_EXTRACT_TIMEOUT_SECS=30    # per-row budget
ONPREM_EXTRACT_MAX_CHARS=6000     # guards the NPU context cap

ONPREM_VERIFY_ENABLED=false       # master switch
ONPREM_VERIFY_TIMEOUT_SECS=45
ONPREM_VERIFY_PASSAGE_CHARS=1200  # per-passage evidence budget
ONPREM_VERIFY_MAX_PASSAGES=6
```

All live in `RouterConfig` (matching the `nl2sql_*` precedent) and are documented in
`.env.example`. The model aliases keep their existing `ONPREM_MODEL_EXTRACTOR` /
`ONPREM_MODEL_VERIFIER` keys and honour persisted per-role overrides via
`AppState::spec_for`.

## Verification

- Server `cargo check` clean, **`cargo test` 71/71 pass** (17 new: 6 extractor, 11
  verifier).
- App `npx tsc --noEmit` clean; bridge `cargo check` clean.
- **Not verified against a live model.** The tool schemas, the grammar-retry path, and
  end-to-end extraction/verification quality have not been exercised against a running
  Foundry instance — that needs `phi-4-mini` and `phi-4-mini-reasoning` downloaded and
  the two flags flipped. This is the main open risk.

## Known limits / follow-ups

1. The verify badge rides on the **pending run**, so it disappears when the
   conversation is refetched from the DB. Persisting the report on the assistant
   message is the natural next step.
2. `/agents/<kind>` semantic answers are **not** verified — only `/chat`.
3. No index on `records.extracted.*`. Filtering by ICD-10 code will table-scan until
   one is added; the catalog does not know the field exists either (that is the
   still-open "ingest-time catalog population" item).
4. Re-running ingest with the extractor newly enabled re-annotates from scratch — there
   is no incremental "annotate what is missing" path.
5. `ThinkFilter` still only strips `<think>`. Fix 2 removes the cause of the
   `<tool_call>` leak rather than filtering the symptom; a model that emits tool-call
   syntax unprompted would still leak.
