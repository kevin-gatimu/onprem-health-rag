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
