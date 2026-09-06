//! Execution provenance: which rungs of the fallback ladder were attempted, in order,
//! and how each one ended (plan 04 §2, plan 04a §9).
//!
//! # Wire format contract (plan 04a §9 — binding)
//!
//! The internal types are data-carrying Rust enums because that is the right Rust for a
//! ladder. The **wire** shape is deliberately *flat*:
//!
//! ```json
//! {"rung": "Link", "result": "miss", "reason": "bind error: no EventTime on Bill"}
//! ```
//!
//! Serde's default (externally tagged) encoding of `Rung::Link(RungResult::Miss(..))`
//! would instead produce `{"Link": {"Miss": "…"}}`, which the bridge mirror cannot read.
//! Because `provenance` is `Option` on both sides, that mismatch **fails silently** — the
//! field just disappears client-side. So `Rung` gets a hand-written `Serialize` that goes
//! through the explicit `RungWire` DTO.
//!
//! Verified against the actual bridge mirrors (not against the plan prose):
//! - `onprem-rag-app/src-tauri/src/commands.rs:2120` — `ProvenanceRung { rung: String,
//!   result: String, reason: Option<String> }` with `skip_serializing_if = "Option::is_none"`.
//! - `onprem-rag-app/src-tauri/src/commands.rs:2132` — `Provenance { path, backend,
//!   service_line, scope, source_id, elapsed_ms: HashMap<String, u64> }`.
//! - `onprem-rag-app/src/lib/bridge.ts:1296` — `ProvenanceRung.result` is narrowed to
//!   `"hit" | "miss" | "skipped"`, hence the lower-case result strings below.
//!
//! # PHI contract (plan 04a §7 — binding)
//!
//! Every string that reaches this module must be schema-shaped, never data-shaped: rung
//! names, column/table names, operator names, timings. A `Miss` reason is **never** built
//! by interpolating a filter literal, because filter literals come from the user's question
//! and may carry a patient name, an MRN, or a national ID. Reason strings are produced by
//! the helpers in this module and by fixed `&'static str` literals at the rung call sites.

// Plan-04 §2 / plan-04a §9 subsystem: execution provenance types (Rung,
// RungResult, Provenance), complete and unit-tested but not yet reachable from
// the live request path. Wiring is gated on the golden suite reaching 55/60
// (currently 28/62); see TODO(plan03-live) in `nl2sql::prepare`. Until then every
// public item here is dead from the binary's point of view, and the resulting
// warning wall drowns out real signal — so the gate is recorded here instead of in
// build output.
#![allow(dead_code)]

use std::collections::HashMap;

use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};

use crate::ontology::service_line::ServiceLine;

/// How one rung of the ladder ended.
///
/// `Miss` carries a **PHI-free** reason (see the module PHI contract). `Skipped` carries a
/// `&'static str` so a skip reason can never be built from user input at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RungResult {
    Hit,
    Miss(String),
    Skipped(&'static str),
}

impl RungResult {
    /// The lower-case discriminant the bridge narrows on (`"hit" | "miss" | "skipped"`).
    pub fn label(&self) -> &'static str {
        match self {
            RungResult::Hit => "hit",
            RungResult::Miss(_) => "miss",
            RungResult::Skipped(_) => "skipped",
        }
    }

    /// The reason string, if this outcome has one. `Hit` has none.
    pub fn reason(&self) -> Option<&str> {
        match self {
            RungResult::Hit => None,
            RungResult::Miss(reason) => Some(reason.as_str()),
            RungResult::Skipped(reason) => Some(*reason),
        }
    }

    /// True for anything that should make the ladder try the next rung.
    pub fn is_miss(&self) -> bool {
        matches!(self, RungResult::Miss(_))
    }
}

/// One attempted rung of the ladder, in the order plan 04 §2.1 defines it.
///
/// `Clarify` is result-less: reaching it *is* the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rung {
    Link(RungResult),
    DeterministicSql(RungResult),
    ModelSql(RungResult),
    Validate(RungResult),
    Execute(RungResult),
    Aggregation(RungResult),
    List(RungResult),
    Retrieval(RungResult),
    Clarify,
}

impl Rung {
    /// Stable rung name on the wire. PascalCase, matching plan 04a §9's example
    /// (`{"rung": "Link", …}`). Also used as the `elapsed_ms` key.
    pub fn name(&self) -> &'static str {
        match self {
            Rung::Link(_) => "Link",
            Rung::DeterministicSql(_) => "DeterministicSql",
            Rung::ModelSql(_) => "ModelSql",
            Rung::Validate(_) => "Validate",
            Rung::Execute(_) => "Execute",
            Rung::Aggregation(_) => "Aggregation",
            Rung::List(_) => "List",
            Rung::Retrieval(_) => "Retrieval",
            Rung::Clarify => "Clarify",
        }
    }

    /// The rung's outcome. `Clarify` has none — reaching it is itself terminal, so it
    /// reports as a `Hit` on the wire rather than inventing a fourth result value the
    /// bridge would not recognise.
    pub fn result(&self) -> &RungResult {
        match self {
            Rung::Link(r)
            | Rung::DeterministicSql(r)
            | Rung::ModelSql(r)
            | Rung::Validate(r)
            | Rung::Execute(r)
            | Rung::Aggregation(r)
            | Rung::List(r)
            | Rung::Retrieval(r) => r,
            Rung::Clarify => &RungResult::Hit,
        }
    }

    /// True when this rung missed and the ladder moved on.
    pub fn is_miss(&self) -> bool {
        self.result().is_miss()
    }
}

/// The flat wire DTO. This is the *only* shape that crosses the process boundary; see the
/// module wire-format contract. Constructed from `Rung` so the enum stays the source of
/// truth.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RungWire<'a> {
    pub rung: &'static str,
    pub result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'a str>,
}

impl<'a> From<&'a Rung> for RungWire<'a> {
    fn from(rung: &'a Rung) -> Self {
        let result = rung.result();
        RungWire {
            rung: rung.name(),
            result: result.label(),
            reason: result.reason(),
        }
    }
}

/// Hand-written so the flat shape survives any refactor of the enum above. A derived
/// `Serialize` here would silently emit the externally-tagged form the bridge cannot read.
impl Serialize for Rung {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = RungWire::from(self);
        // `reason` is skipped when absent, so the struct length differs by outcome.
        let len = if wire.reason.is_some() { 3 } else { 2 };
        let mut s = serializer.serialize_struct("ProvenanceRung", len)?;
        s.serialize_field("rung", wire.rung)?;
        s.serialize_field("result", wire.result)?;
        if let Some(reason) = wire.reason {
            s.serialize_field("reason", reason)?;
        }
        s.end()
    }
}

/// Which backend actually produced the answer. Mirrors `bridge.ts` `Provenance.backend`.
pub const BACKEND_SOURCE_SQL: &str = "source_sql";
pub const BACKEND_DOCUMENT_DB: &str = "document_db";
pub const BACKEND_SEMANTIC: &str = "semantic";
pub const BACKEND_HYBRID: &str = "hybrid";
pub const BACKEND_NONE: &str = "none";

/// Execution provenance for one turn. Emitted as the `provenance` SSE event before the
/// first `token`, and persisted alongside the assistant message.
#[derive(Debug, Clone, Serialize)]
pub struct Provenance {
    /// Ordered attempts, e.g. `[DeterministicSql(Hit)]`.
    pub path: Vec<Rung>,
    pub backend: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_line: Option<ServiceLine>,
    pub scope: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Per-rung wall time, keyed by `Rung::name`.
    pub elapsed_ms: HashMap<&'static str, u64>,
}

impl Default for Provenance {
    fn default() -> Self {
        Provenance {
            path: Vec::new(),
            backend: BACKEND_NONE,
            service_line: None,
            scope: Vec::new(),
            source_id: None,
            elapsed_ms: HashMap::new(),
        }
    }
}

impl Provenance {
    /// Record one attempted rung plus how long it took.
    pub fn push(&mut self, rung: Rung, elapsed_ms: u64) {
        self.elapsed_ms.insert(rung.name(), elapsed_ms);
        self.path.push(rung);
    }

    /// Record a rung with no meaningful duration (a skip, or a decision).
    pub fn note(&mut self, rung: Rung) {
        self.path.push(rung);
    }

    /// True when at least one rung missed — i.e. the answer came from a fallback.
    /// Plan 04 §9 requires that such answers never carry an empty path.
    pub fn had_miss(&self) -> bool {
        self.path.iter().any(Rung::is_miss)
    }

    /// True when any rung was skipped for budget (plan 04a §6 — a budget skip must be
    /// visible in the response, not silently downgrade the answer).
    pub fn had_budget_skip(&self) -> bool {
        self.path
            .iter()
            .any(|r| matches!(r.result(), RungResult::Skipped("budget")))
    }

    /// Compact JSON for the SSE `provenance` event and for persistence.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// ---------------------------------------------------------------------------
// PHI-free reason builders.
//
// Every `Miss` reason in the executor comes from one of these (or a `&'static str`).
// They interpolate schema identifiers only — never a filter value, never a row value.
// ---------------------------------------------------------------------------

/// A bind failure. `detail` must name schema, e.g. "no EventTime on Bill".
pub fn bind_error(detail: &str) -> String {
    format!("bind error: {detail}")
}

/// An AST-validation rejection. `detail` comes from `nl2sql::validate`, which reports
/// table/column names and rule names, never literals.
pub fn validation_error(detail: &str) -> String {
    format!("validation error: {detail}")
}

/// A connector/driver failure. `detail` is the *class* of error, not the driver's raw
/// message, because driver messages can echo bound parameter values.
pub fn connector_error(detail: &str) -> String {
    format!("connector error: {detail}")
}

/// A per-rung deadline expiry.
pub fn timeout(rung: &str, secs: u64) -> String {
    format!("timeout: {rung} exceeded {secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// Plan 04a §9: assert on the **JSON**, not on a Rust round-trip. A round-trip through
    /// our own types passes under any encoding, including the wrong one.
    #[test]
    fn miss_serialises_to_the_flat_bridge_shape() {
        let rung = Rung::Link(RungResult::Miss(bind_error("no EventTime on Bill")));
        let text = serde_json::to_string(&rung).expect("serialise");

        // Exact keys, exact values. This is the shape commands.rs:2120 deserialises.
        let value: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(
            value,
            json!({
                "rung": "Link",
                "result": "miss",
                "reason": "bind error: no EventTime on Bill",
            }),
            "provenance rung must be flat {{rung, result, reason}}; got {text}"
        );

        // Guard the specific wrong encoding serde would produce by default.
        assert!(
            !text.contains("\"Link\":"),
            "externally-tagged encoding leaked: {text}"
        );
        assert!(
            !text.contains("\"Miss\""),
            "externally-tagged encoding leaked: {text}"
        );
    }

    #[test]
    fn hit_serialises_without_a_reason_key() {
        let rung = Rung::DeterministicSql(RungResult::Hit);
        let text = serde_json::to_string(&rung).expect("serialise");
        let value: Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(
            value,
            json!({ "rung": "DeterministicSql", "result": "hit" }),
            "got {text}"
        );
        assert!(
            !text.contains("reason"),
            "Hit must omit `reason` entirely (the bridge skips it too): {text}"
        );
    }

    #[test]
    fn skipped_serialises_with_its_static_reason() {
        let rung = Rung::Aggregation(RungResult::Skipped("budget"));
        let value: Value = serde_json::to_value(&rung).expect("serialise");
        assert_eq!(
            value,
            json!({ "rung": "Aggregation", "result": "skipped", "reason": "budget" })
        );
    }

    /// Every variant must carry a stable, unique wire name.
    #[test]
    fn every_rung_name_is_stable_and_unique() {
        let all = vec![
            Rung::Link(RungResult::Hit),
            Rung::DeterministicSql(RungResult::Hit),
            Rung::ModelSql(RungResult::Hit),
            Rung::Validate(RungResult::Hit),
            Rung::Execute(RungResult::Hit),
            Rung::Aggregation(RungResult::Hit),
            Rung::List(RungResult::Hit),
            Rung::Retrieval(RungResult::Hit),
            Rung::Clarify,
        ];
        let mut names: Vec<&str> = all.iter().map(Rung::name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate rung name on the wire");
        assert_eq!(
            Rung::Clarify.result(),
            &RungResult::Hit,
            "Clarify must report a result the bridge recognises"
        );
    }

    #[test]
    fn provenance_document_matches_the_bridge_mirror_keys() {
        let mut prov = Provenance {
            backend: BACKEND_SOURCE_SQL,
            service_line: Some(ServiceLine::Maternity),
            scope: vec!["admissions".to_string()],
            source_id: Some("src-1".to_string()),
            ..Default::default()
        };
        prov.push(Rung::Link(RungResult::Hit), 4);
        prov.push(
            Rung::DeterministicSql(RungResult::Miss(validation_error("unknown column"))),
            7,
        );

        let value: Value = serde_json::from_str(&prov.to_json()).expect("parse");
        let obj = value.as_object().expect("object");
        // commands.rs:2132 field set.
        for key in [
            "path",
            "backend",
            "service_line",
            "scope",
            "source_id",
            "elapsed_ms",
        ] {
            assert!(obj.contains_key(key), "missing `{key}` in {value}");
        }
        assert_eq!(obj["backend"], json!("source_sql"));
        // ServiceLine serialises snake_case; the bridge types it as Option<String>.
        assert_eq!(obj["service_line"], json!("maternity"));
        assert_eq!(obj["elapsed_ms"]["Link"], json!(4));
        assert_eq!(obj["path"][1]["result"], json!("miss"));
        assert!(prov.had_miss());
    }

    /// Plan 04a §7: the same class of guard as `no_pii_enum_values_in_dev_seed`. The reason
    /// builders must add no data of their own, and an assembled provenance for a
    /// PHI-bearing question must stay clean.
    #[test]
    fn provenance_reasons_never_carry_phi_values() {
        // Values shaped like the ones that appear in a user's question / the fixtures.
        const PHI: &[&str] = &[
            "Wanjiku",
            "MRN-00042",
            "12345678",
            "1990-04-11",
            "+254712345678",
        ];

        let mut prov = Provenance {
            backend: BACKEND_SEMANTIC,
            scope: vec!["admissions".to_string(), "clinical_notes".to_string()],
            ..Default::default()
        };
        prov.push(
            Rung::Link(RungResult::Miss(bind_error("no PatientRef on Bill"))),
            3,
        );
        prov.push(
            Rung::DeterministicSql(RungResult::Miss(validation_error(
                "column patients.secret is not in the allow-list",
            ))),
            5,
        );
        prov.push(
            Rung::Execute(RungResult::Miss(connector_error("pool timeout"))),
            9,
        );
        prov.push(Rung::Aggregation(RungResult::Skipped("budget")), 0);
        prov.push(Rung::Retrieval(RungResult::Hit), 40);

        let text = prov.to_json();
        for phi in PHI {
            assert!(
                !text.contains(phi),
                "provenance leaked a PHI-shaped value `{phi}`: {text}"
            );
        }

        // And the builders themselves add no data beyond their prefix.
        assert_eq!(
            bind_error("no EventTime on Bill"),
            "bind error: no EventTime on Bill"
        );
        assert_eq!(timeout("sql_exec", 30), "timeout: sql_exec exceeded 30s");
        assert!(prov.had_budget_skip());
    }
}
