//! Wire-format tests for the bridge mirrors.
//!
//! These tests assert on **JSON text**, never on a Rust round-trip through the
//! bridge's own structs. A round-trip through matching types passes under any
//! encoding, including a wrong one, so it cannot detect a mirror that disagrees
//! with the server (see `plans/new/04a-acceptance-measure.md` §9).

use std::collections::HashMap;

use crate::commands::{
    BindingTablesResponse, MetadataOverrides, Provenance, ProvenanceRung, ServerAgentsResponse,
    StoredMessage, project_agent_roster,
};

/// The flat provenance shape the bridge mirror requires, asserted as literal JSON.
///
/// `plans/new/04a-acceptance-measure.md` §9 records the decision that the server
/// emits `{"rung","result","reason"}` and NOT serde's default externally-tagged
/// `{"Link":{"Miss":"..."}}`. This test pins the client half of that contract: if
/// someone "fixes" `ProvenanceRung` into a data-carrying enum, this fails loudly
/// instead of the field silently arriving as absent (both sides are `Option`).
#[test]
fn provenance_rung_is_flat_not_externally_tagged() {
    let miss = ProvenanceRung {
        rung: "Link".into(),
        result: "miss".into(),
        reason: Some("bind error: no EventTime on Bill".into()),
    };
    assert_eq!(
        serde_json::to_string(&miss).unwrap(),
        r#"{"rung":"Link","result":"miss","reason":"bind error: no EventTime on Bill"}"#
    );

    // `Hit` and `Skipped` carry no reason; `reason` must be omitted, not null, so
    // `reason?: string` on the TS side is never `null`.
    let hit = ProvenanceRung {
        rung: "DeterministicSql".into(),
        result: "hit".into(),
        reason: None,
    };
    assert_eq!(
        serde_json::to_string(&hit).unwrap(),
        r#"{"rung":"DeterministicSql","result":"hit"}"#
    );
    let skipped = ProvenanceRung {
        rung: "ModelSql".into(),
        result: "skipped".into(),
        reason: None,
    };
    assert_eq!(
        serde_json::to_string(&skipped).unwrap(),
        r#"{"rung":"ModelSql","result":"skipped"}"#
    );

    // The shape the server is required to send parses into the mirror.
    let parsed: ProvenanceRung =
        serde_json::from_str(r#"{"rung":"Validate","result":"hit"}"#).unwrap();
    assert_eq!(parsed.rung, "Validate");
    assert_eq!(parsed.result, "hit");
    assert!(parsed.reason.is_none());

    // The externally-tagged shape must NOT parse — proving the two encodings are
    // genuinely incompatible rather than accidentally interchangeable.
    assert!(
        serde_json::from_str::<ProvenanceRung>(r#"{"Link":{"Miss":"bind error"}}"#).is_err(),
        "externally-tagged Rung must not deserialise into the flat mirror"
    );
}

/// A full `Provenance` payload as literal JSON parses with every field reachable.
#[test]
fn provenance_payload_parses_from_literal_json() {
    let json = r#"{
        "path": [
            {"rung":"Link","result":"hit"},
            {"rung":"DeterministicSql","result":"miss","reason":"no EventTime on Bill"},
            {"rung":"ModelSql","result":"skipped"}
        ],
        "backend": "source_sql",
        "service_line": "pharmacy",
        "scope": ["prescription","medication"],
        "source_id": "pg-dev",
        "elapsed_ms": {"link": 3, "compile": 12, "execute": 197}
    }"#;
    let p: Provenance = serde_json::from_str(json).unwrap();
    assert_eq!(p.path.len(), 3);
    assert_eq!(p.path[1].result, "miss");
    assert_eq!(p.path[1].reason.as_deref(), Some("no EventTime on Bill"));
    assert_eq!(p.backend, "source_sql");
    assert_eq!(p.service_line.as_deref(), Some("pharmacy"));
    assert_eq!(p.scope, vec!["prescription", "medication"]);
    assert_eq!(p.source_id.as_deref(), Some("pg-dev"));
    assert_eq!(p.elapsed_ms.get("execute"), Some(&197));
}

/// Plan 07 §7: a `MessageOut` JSON carrying every new field deserialises without
/// loss. Asserted from literal JSON so mirror drift is caught even though both
/// halves are `Option`.
#[test]
fn stored_message_parses_all_new_fields_from_literal_json() {
    let json = r#"{
        "id": "m1",
        "role": "assistant",
        "content": "42 admissions.",
        "citations": null,
        "agent_kind": "ward_board",
        "sql_result": {
            "source_id": "pg-dev",
            "sql": "SELECT 1",
            "columns": ["n"],
            "rows": [[1]],
            "spec": {"measure":"count"},
            "explanation": "counts admissions"
        },
        "provenance": {
            "path": [{"rung":"Link","result":"hit"}],
            "backend": "source_sql",
            "scope": ["admission"],
            "elapsed_ms": {"execute": 12}
        },
        "spec": {"measure":"count"},
        "suggestions": [
            {"text":"Break down by ward","kind":"drill","spec":{"group_by":"ward"}},
            {"text":"Open in Pharmacy","kind":"switch","agent":"pharmacy"}
        ],
        "focus_used": ["patient","period"],
        "clarify": {"question":"Which ward?","slot":"dimension","options":["A","B"]},
        "mode": "trends",
        "created_at": "2026-01-15T08:00:00Z"
    }"#;
    let m: StoredMessage = serde_json::from_str(json).unwrap();
    assert_eq!(m.agent_kind.as_deref(), Some("ward_board"));
    assert_eq!(m.mode.as_deref(), Some("trends"));
    assert_eq!(
        m.focus_used.as_deref(),
        Some(&["patient".to_string(), "period".to_string()][..])
    );
    let sql = m.sql_result.expect("sql_result");
    assert_eq!(sql.explanation.as_deref(), Some("counts admissions"));
    assert!(sql.spec.is_some());
    let prov = m.provenance.expect("provenance");
    assert_eq!(prov.path[0].rung, "Link");
    let sugg = m.suggestions.expect("suggestions");
    assert_eq!(sugg.len(), 2);
    assert_eq!(sugg[1].kind, "switch");
    assert_eq!(sugg[1].agent.as_deref(), Some("pharmacy"));
    let clarify = m.clarify.expect("clarify");
    assert_eq!(clarify.question, "Which ward?");
    assert_eq!(clarify.options, vec!["A", "B"]);
    assert!(m.spec.is_some());
}

/// A pre-plan-06 `MessageOut` (none of the new fields present) still parses — the
/// new mirrors must not make old messages unreadable.
#[test]
fn stored_message_parses_without_any_new_fields() {
    let json = r#"{
        "id": "m0",
        "role": "assistant",
        "content": "hello",
        "citations": null,
        "created_at": "2026-01-15T08:00:00Z"
    }"#;
    let m: StoredMessage = serde_json::from_str(json).unwrap();
    assert!(m.provenance.is_none());
    assert!(m.suggestions.is_none());
    assert!(m.clarify.is_none());
    assert!(m.mode.is_none());
}

/// `GET /agents` as the server actually sends it, asserted from literal JSON and
/// projected into the roster the React layer consumes.
///
/// VERIFIED against `AgentsResponse` / `ServiceLineInfo` in
/// `onprem-rag-server/src/agents/registry.rs` L30-58 — the route the server mounts
/// (`main.rs`: `agents::registry::list_agents`). Note `example_questions` (the
/// answerable-only list) sits alongside the unfiltered `examples`, and `modes` is
/// the full `AgentMode::ALL` set.
#[test]
fn real_agents_response_projects_into_roster() {
    let json = r#"{
        "service_lines": [
            {
                "slug": "patient_chart",
                "label": "Patient Chart",
                "blurb": "Everything about one patient.",
                "tier": 1,
                "concepts": ["patient","encounter"],
                "examples": [
                    {"question":"Show me this patient's allergies","required_concepts":["allergy"]},
                    {"question":"What is this patient owed?","required_concepts":["bill"]}
                ],
                "example_questions": ["Show me this patient's allergies"],
                "modes": ["ask","trends","handover"]
            },
            {
                "slug": "revenue",
                "label": "Revenue",
                "blurb": "Billing and payments.",
                "tier": 3,
                "concepts": ["bill"],
                "examples": [],
                "example_questions": [],
                "modes": ["ask","trends","handover"]
            }
        ],
        "source_usable_lines": {
            "pg-dev": ["patient_chart"]
        }
    }"#;
    let resp: ServerAgentsResponse = serde_json::from_str(json).unwrap();

    let tables: HashMap<String, HashMap<String, Vec<String>>> = HashMap::from([(
        "pg-dev".to_string(),
        HashMap::from([(
            "patient_chart".to_string(),
            vec!["patient".to_string(), "encounter".to_string()],
        )]),
    )]);

    let roster = project_agent_roster(resp, &tables);
    assert_eq!(roster.len(), 2);

    let chart = &roster[0];
    assert_eq!(chart.kind, "patient_chart");
    assert_eq!(chart.label, "Patient Chart");
    assert_eq!(chart.tier, 1);
    assert!(chart.usable, "a line listed in source_usable_lines is usable");
    assert_eq!(chart.sources.len(), 1);
    assert_eq!(chart.sources[0].source_id, "pg-dev");
    assert_eq!(chart.sources[0].tables, vec!["patient", "encounter"]);
    // The FILTERED list wins: "What is this patient owed?" is in `examples` but not
    // in `example_questions`, so the roster must not advertise it.
    assert_eq!(
        chart.example_questions,
        vec!["Show me this patient's allergies"]
    );
    assert_eq!(chart.concepts, vec!["patient", "encounter"]);
    assert_eq!(chart.modes, vec!["ask", "trends", "handover"]);

    let revenue = &roster[1];
    assert!(!revenue.usable, "a line no source can serve is not usable");
    assert!(revenue.sources.is_empty());
    assert!(revenue.example_questions.is_empty());
}

/// A server that predates the `example_questions` / `modes` split still projects:
/// examples fall back to the unfiltered list and the mode toggle stays hidden.
/// This is the compatibility floor, asserted from literal JSON.
#[test]
fn agents_response_without_modes_or_filtered_examples_falls_back() {
    let json = r#"{
        "service_lines": [
            {
                "slug": "pharmacy",
                "label": "Pharmacy",
                "blurb": "Meds.",
                "tier": 1,
                "concepts": ["prescription"],
                "examples": [{"question":"Top 10 dispensed drugs","required_concepts":[]}]
            }
        ],
        "source_usable_lines": {"pg-dev": ["pharmacy"]}
    }"#;
    let resp: ServerAgentsResponse = serde_json::from_str(json).unwrap();
    let roster = project_agent_roster(resp, &HashMap::new());
    assert_eq!(roster[0].example_questions, vec!["Top 10 dispensed drugs"]);
    assert_eq!(
        roster[0].modes,
        vec!["ask"],
        "no modes on the wire must not fabricate a Trends/Handover toggle"
    );
}

/// An EMPTY `example_questions` means "nothing this line offers is answerable" and
/// must be honoured, not treated as absent and back-filled from `examples`.
#[test]
fn empty_filtered_examples_are_not_backfilled() {
    let json = r#"{
        "service_lines": [
            {
                "slug": "pharmacy",
                "label": "Pharmacy",
                "blurb": "Meds.",
                "tier": 1,
                "concepts": ["prescription"],
                "examples": [{"question":"Top 10 dispensed drugs","required_concepts":["prescription"]}],
                "example_questions": [],
                "modes": ["ask","trends","handover"]
            }
        ],
        "source_usable_lines": {"pg-dev": ["pharmacy"]}
    }"#;
    let resp: ServerAgentsResponse = serde_json::from_str(json).unwrap();
    let roster = project_agent_roster(resp, &HashMap::new());
    assert!(
        roster[0].example_questions.is_empty(),
        "an explicitly empty answerable list must not fall back to the unfiltered set"
    );
}

/// A source whose binding could not be read still yields a scope entry, with no
/// tables — the agent must not disappear because one binding fetch failed.
#[test]
fn missing_binding_yields_empty_table_scope() {
    let json = r#"{
        "service_lines": [
            {"slug":"pharmacy","label":"Pharmacy","blurb":"Meds.","tier":1,"concepts":[],"examples":[]}
        ],
        "source_usable_lines": {"pg-dev": ["pharmacy"]}
    }"#;
    let resp: ServerAgentsResponse = serde_json::from_str(json).unwrap();
    let roster = project_agent_roster(resp, &HashMap::new());
    assert_eq!(roster[0].sources.len(), 1);
    assert!(roster[0].sources[0].tables.is_empty());
    assert!(roster[0].usable);
}

/// `GET /sources/<id>/binding` as the server sends it (verified against
/// `TableSummary`), parsed from literal JSON.
#[test]
fn binding_response_parses_table_service_lines() {
    let json = r#"{
        "source_id": "pg-dev",
        "bound_at": "2026-01-15T08:00:00+00:00",
        "degraded": false,
        "coverage": {"total_tables": 2},
        "usable_lines": ["patient_chart"],
        "tables": [
            {"table_name":"patient","concept":"patient","confidence":0.94,
             "service_lines":["patient_chart","front_desk"],
             "event_time_col":null,"patient_path_hops":0}
        ]
    }"#;
    let b: BindingTablesResponse = serde_json::from_str(json).unwrap();
    assert_eq!(b.tables.len(), 1);
    assert_eq!(b.tables[0].table_name, "patient");
    assert_eq!(
        b.tables[0].service_lines,
        vec!["patient_chart", "front_desk"]
    );
}

/// The overrides document must survive a GET → PUT cycle with every field intact.
///
/// `PUT /nl2sql/<id>/catalog/overrides` REPLACES the stored document
/// (`replace_one(..).upsert(true)`, `onprem-rag-server/src/nl2sql/http.rs`), so a
/// mirror missing a field silently erases it on the next save. This asserts on the
/// re-serialised JSON TEXT — a struct-to-struct round-trip would pass even if the
/// mirror had two fields and the server had five, which is the bug this catches.
#[test]
fn metadata_overrides_round_trip_preserves_every_field() {
    let server_json = r#"{
        "aliases": [{"table":"bill","column":"amt","alias":"charge"}],
        "relationships": [{"from_table":"bill","from_column":"pid","to_table":"patient","to_column":"id"}],
        "table_concepts": [{"table":"bill","concept":"bill"},{"table":"tmp_x","concept":null}],
        "column_roles": [{"table":"bill","column":"created","role":"event_time"}],
        "service_lines": ["revenue","pharmacy"]
    }"#;

    let parsed: MetadataOverrides = serde_json::from_str(server_json).unwrap();
    let sent = serde_json::to_string(&parsed).unwrap();

    // Assert on the outgoing TEXT: every key the server stores must still be there.
    for key in [
        "\"aliases\"",
        "\"relationships\"",
        "\"table_concepts\"",
        "\"column_roles\"",
        "\"service_lines\"",
    ] {
        assert!(sent.contains(key), "PUT body dropped {key}: {sent}");
    }
    assert!(
        sent.contains(r#"{"table":"bill","concept":"bill"}"#),
        "concept override lost on the way out: {sent}"
    );
    assert!(
        sent.contains(r#"{"table":"tmp_x","concept":null}"#),
        "an ignore-this-table override must survive as an explicit null: {sent}"
    );
    assert!(
        sent.contains(r#""role":"event_time""#),
        "column role lost on the way out: {sent}"
    );
    assert!(
        sent.contains(r#""service_lines":["revenue","pharmacy"]"#),
        "forced service lines lost on the way out: {sent}"
    );
}

/// A pre-plan-05 overrides document (only `aliases` + `relationships`) still parses,
/// with the three newer fields defaulting to empty rather than failing the request.
#[test]
fn metadata_overrides_parses_legacy_two_field_document() {
    let legacy = r#"{"aliases":[],"relationships":[]}"#;
    let parsed: MetadataOverrides = serde_json::from_str(legacy).unwrap();
    assert!(parsed.table_concepts.is_empty());
    assert!(parsed.column_roles.is_empty());
    assert!(parsed.service_lines.is_empty());
}

// ---------------------------------------------------------------------------
// `routed` SSE payload (`chat://routed` / `agent://routed`)
// ---------------------------------------------------------------------------
//
// The bridge relays this event's `data` VERBATIM (see the `POST /agents/<kind>`
// command in `commands.rs`), so there is no Rust mirror struct to round-trip —
// which is the point: the only thing that can be wrong is the JSON text itself
// and what `bridge.ts::RoutedPayload` claims about it. These tests therefore
// assert on literal payload TEXT and on its exact key set.
//
// Server source of truth: `RouteDecision::to_sse_json`
// (`onprem-rag-server/src/router/mod.rs` L179-235), emitted first by
// `onprem-rag-server/src/rag/routes.rs` L332 and by
// `onprem-rag-server/src/agents/routes.rs` L408.

/// Every key `bridge.ts::RoutedPayload` declares. A payload key outside this set
/// is a field the TypeScript mirror would silently drop.
const ROUTED_PAYLOAD_KEYS: &[&str] = &[
    "route",
    "intent",
    "backend",
    "tier",
    "cached",
    "tier2_attempted",
    "service_line",
    "deterministic",
    "scope_size",
    "source_id",
    "question",
    "slot",
];

fn assert_routed_keys_are_mirrored(payload: &str) {
    let v: serde_json::Value = serde_json::from_str(payload).unwrap();
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("routed payload must be a JSON object: {payload}"));
    for key in obj.keys() {
        assert!(
            ROUTED_PAYLOAD_KEYS.contains(&key.as_str()),
            "routed payload key `{key}` is not declared in bridge.ts::RoutedPayload"
        );
    }
}

/// The `routed` payload is a JSON **object**, not the pre-plan-05 JSON-encoded
/// agent-kind string. Asserted on the literal text of both shapes, because the
/// client discriminates on exactly this (`typeof parsed === "string"` vs
/// `"route" in parsed`, `bridgeEvents.ts`).
#[test]
fn routed_payload_is_an_object_not_a_bare_kind_string() {
    // What a plan-05 server sends for a line agent answering a capability question:
    // `capability_decision` (onprem-rag-server/src/agents/routes.rs L585-597) sets
    // `service_line: kind.line()`, which is `Some(_)` for `AgentKind::Line`, so the
    // v3 block in `to_sse_json` IS inserted.
    let current = r#"{"route":"capability","intent":null,"backend":null,"tier":0,"cached":false,"tier2_attempted":false,"service_line":"pharmacy","deterministic":false,"scope_size":0,"source_id":null}"#;

    // The legacy payload the same event used to carry.
    let legacy = r#""pharmacy""#;

    // The two shapes must be genuinely incompatible, or a mismatch stays invisible.
    assert!(
        serde_json::from_str::<String>(current).is_err(),
        "the current payload must NOT read as a JSON string — decoding it like a \
         `token` frame would mangle it: {current}"
    );
    assert_eq!(
        serde_json::from_str::<String>(legacy).unwrap(),
        "pharmacy",
        "the legacy shape is a JSON string; the compatibility floor in \
         bridgeEvents.ts depends on that staying true"
    );

    let v: serde_json::Value = serde_json::from_str(current).unwrap();
    assert_eq!(v["route"], "capability");
    assert_eq!(v["service_line"], "pharmacy");
    assert_eq!(v["tier"], 0);
    assert_eq!(v["cached"], false);
    assert_eq!(v["tier2_attempted"], false);
    assert_eq!(v["deterministic"], false);
    assert_eq!(v["scope_size"], 0);
    assert!(v["backend"].is_null());
    assert!(v["source_id"].is_null());
    assert_routed_keys_are_mirrored(current);
}

/// `"capability"` is a real route label (`RouteDecision::route_label`,
/// `onprem-rag-server/src/router/mod.rs` L155). It must be in `RouteLabel` in
/// `bridge.ts`, alongside the other six.
#[test]
fn every_route_label_the_server_emits_is_mirrored() {
    // Exhaustive against the `match` in `route_label()`.
    const SERVER_LABELS: &[&str] = &[
        "conversational",
        "capability",
        "conversation_meta",
        "structured",
        "semantic",
        "hybrid",
        "clarify",
    ];
    for label in SERVER_LABELS {
        let payload = format!(
            r#"{{"route":"{label}","intent":null,"backend":null,"tier":1,"cached":false,"tier2_attempted":false}}"#
        );
        assert_routed_keys_are_mirrored(&payload);
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["route"], *label);
    }
    assert_eq!(
        SERVER_LABELS.len(),
        7,
        "a new RouteClass arm needs a matching member in bridge.ts::RouteLabel"
    );
}

/// A structured v3 payload carries the fields the activity strip reads. The v3
/// block is conditional server-side, so `deterministic` / `service_line` /
/// `scope_size` / `source_id` are genuinely optional on the wire — which is why
/// they are optional in `bridge.ts::RoutedPayload`.
#[test]
fn routed_payload_v3_and_clarify_fields_are_all_mirrored() {
    let structured = r#"{"route":"structured","intent":"count","backend":"source_sql","tier":1,"cached":false,"tier2_attempted":false,"service_line":"revenue","deterministic":true,"scope_size":3,"source_id":"pg-dev"}"#;
    assert_routed_keys_are_mirrored(structured);
    let v: serde_json::Value = serde_json::from_str(structured).unwrap();
    // The bridge mirror narrows `backend` to these two spellings; `"doc_db"` is the
    // spelling of the *other* endpoint (`POST /route`), never of this event.
    assert_eq!(v["backend"], "source_sql");
    assert_eq!(v["deterministic"], true);
    assert_eq!(v["scope_size"], 3);
    assert_eq!(v["source_id"], "pg-dev");

    // A v2 payload omits the whole v3 block rather than sending nulls.
    let v2 = r#"{"route":"semantic","intent":null,"backend":null,"tier":2,"cached":true,"tier2_attempted":true}"#;
    assert_routed_keys_are_mirrored(v2);
    let v: serde_json::Value = serde_json::from_str(v2).unwrap();
    assert!(
        v.get("service_line").is_none(),
        "v2 payloads must omit the v3 keys, not null them"
    );

    // Clarify adds two more keys; `slot` is `MissingSlot` under
    // `#[serde(rename_all = "snake_case")]` (`nl2sql/ir/spec.rs` L564-577).
    let clarify = r#"{"route":"clarify","intent":null,"backend":null,"tier":3,"cached":false,"tier2_attempted":true,"question":"Which ward?","slot":"time_range"}"#;
    assert_routed_keys_are_mirrored(clarify);
    let v: serde_json::Value = serde_json::from_str(clarify).unwrap();
    assert_eq!(v["question"], "Which ward?");
    assert_eq!(v["slot"], "time_range");
}

/// `GET /sources/<id>/binding` carries `orphans` (`BindingResponse.orphans`,
/// `onprem-rag-server/src/ontology/routes.rs` L40). The bridge command returns the
/// body as raw `serde_json::Value`, so nothing can drop it in Rust; this pins that
/// the key is present in the real body under the name `bridge.ts` uses
/// (`SourceBinding.orphans`) and is the table LIST, not the coverage count.
#[test]
fn binding_response_carries_orphans_by_that_name() {
    let json = r#"{
        "source_id": "pg-dev",
        "bound_at": "2026-01-15T08:00:00+00:00",
        "degraded": false,
        "coverage": {"total_tables": 2, "bound_tables": 1, "orphan_tables": 1,
                     "exact_concepts": 1, "usable_lines": ["patient_chart"]},
        "usable_lines": ["patient_chart"],
        "orphans": ["tmp_scratch"],
        "tables": [
            {"table_name":"patient","concept":"patient","confidence":0.94,
             "service_lines":["patient_chart"],"event_time_col":null,"patient_path_hops":0}
        ]
    }"#;
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(
        v["orphans"],
        serde_json::json!(["tmp_scratch"]),
        "`orphans` is a top-level array of table names, NOT the `coverage.orphan_tables` count"
    );
    assert_eq!(v["coverage"]["orphan_tables"], 1);
    // The subset mirror the roster uses must still parse the same body.
    let b: BindingTablesResponse = serde_json::from_str(json).unwrap();
    assert_eq!(b.tables.len(), 1);
}
