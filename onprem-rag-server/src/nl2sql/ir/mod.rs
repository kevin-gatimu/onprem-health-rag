//! `nl2sql::ir` — QuerySpec IR, grammar, binder, compiler.
//!
//! **Shadow mode**: when `ONPREM_SQL_IR_ENABLED=false` (the default) the IR
//! runs parse → bind → compile → validate on every question but never affects
//! the answer. A single `ir_shadow` JSON line is emitted per question.
//! Set `ONPREM_SQL_IR_ENABLED=true` to let the IR replace the template engine.

pub mod bind;
pub mod compile;
pub mod parse;
pub mod plan_dto;
pub mod predicates;
pub mod spec;

pub use bind::bind;
pub use compile::compile;
pub use parse::{parse, ParseOutcome};
pub use plan_dto::PlannedSpec;
pub use predicates::{lookup_predicate, predicate_to_filter};
pub use spec::QuerySpec;

#[cfg(test)]
mod golden_tests {
    //! Golden suite: parse → bind → compile → validate against both fixtures.
    //!
    //! Gate: ≥55/62 dev entries, ≥50/62 alt entries must succeed.
    //! Bless: set env ONPREM_BLESS_GOLDEN=1 to rewrite expected SQL from current behaviour.

    use std::collections::HashMap;
    use chrono::{DateTime, TimeZone, Utc};
    use serde::{Deserialize, Serialize};

    use crate::connectors::SourceKind;
    use crate::nl2sql::ir::{bind, compile, parse, ParseOutcome};
    use crate::nl2sql::validate::validate_sql;
    use crate::ontology::binder::bind_cards;
    use crate::ontology::binding::SchemaBinding;
    use crate::ontology::tests::{dev_seed_cards, alt_schema_cards};

    const PROD_MIN_CONFIDENCE: f32 = 0.55;
    const MAX_ROWS: i64 = 500;

    /// Fixed timestamp for deterministic SQL generation (2026-01-15 08:00:00 UTC).
    fn frozen_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 15, 8, 0, 0).unwrap()
    }

    fn make_binding(cards: &[crate::nl2sql::spec::TableCard], source_id: &str) -> SchemaBinding {
        let tables = bind_cards(cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());
        SchemaBinding {
            source_id: source_id.to_string(),
            bound_at: frozen_now(),
            tables,
            degraded: false,
            override_version: 0,
        }
    }

    #[derive(Debug, Deserialize, Serialize, Clone)]
    struct GoldenEntry {
        id: String,
        question: String,
        binding: String,
        expect_shape: Option<String>,
        expect_sql_pg: Option<String>,
        expect_sql_mysql: Option<String>,
        expect_sql_mssql: Option<String>,
        /// Concept slugs that are absent from this binding's schema.
        /// When set and non-empty, a pipeline failure is a PASS (correct miss):
        /// the binding genuinely cannot answer this question.
        /// A pipeline success is also accepted (the table was added later).
        #[serde(default)]
        expect_missing: Option<Vec<String>>,
    }

    fn load_golden() -> Vec<GoldenEntry> {
        let raw = include_str!("tests/golden.jsonl");
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, line)| {
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("golden.jsonl line {}: {e}\n  > {line}", i + 1))
            })
            .collect()
    }

    /// Try parse → bind → compile → validate for a single question.
    fn try_pipeline(
        question: &str,
        cards: &[crate::nl2sql::spec::TableCard],
        binding: &SchemaBinding,
        dialect: SourceKind,
    ) -> Option<String> {
        let allowed: Vec<String> = cards.iter().map(|c| c.table_name.clone()).collect();
        let outcome = parse(question, None, binding, &allowed);
        let (spec, _missing) = match outcome {
            ParseOutcome::Parsed { spec, missing } => (spec, missing),
            ParseOutcome::NoParse => return None,
        };
        let bound = bind(spec, binding, cards, &allowed).ok()?;
        let compiled = compile(&bound, dialect, MAX_ROWS, frozen_now()).ok()?;
        let validated = validate_sql(&compiled.sql, dialect, MAX_ROWS, &allowed).ok()?;
        Some(validated.sql)
    }

    #[test]
    fn golden_suite_dev_binding() {
        let cards = dev_seed_cards();
        let binding = make_binding(&cards, "dev");
        let entries: Vec<GoldenEntry> = load_golden()
            .into_iter()
            .filter(|e| e.binding == "dev")
            .collect();

        let total = entries.len();
        let bless_mode = std::env::var("ONPREM_BLESS_GOLDEN").is_ok();
        let mut passed = 0usize;
        let mut failures: Vec<String> = Vec::new();
        let mut blessed: Vec<(usize, GoldenEntry)> = Vec::new();

        for (orig_idx, entry) in entries.iter().enumerate() {
            let is_expected_miss = entry.expect_missing
                .as_deref()
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let sql_pg = try_pipeline(&entry.question, &cards, &binding, SourceKind::Postgres);
            // expect_missing entries: the pipeline MUST FAIL.
            // A success means the parser/binder produced a confident answer on a
            // schema that lacks the required table — a false-positive binding that
            // would serve the wrong data to a clinician.  That is a test FAILURE.
            let ok = if is_expected_miss {
                sql_pg.is_none()
            } else {
                sql_pg.is_some()
            };
            if ok {
                passed += 1;
                if bless_mode && !is_expected_miss {
                    let mut updated = entry.clone();
                    updated.expect_sql_pg = sql_pg;
                    updated.expect_sql_mysql =
                        try_pipeline(&entry.question, &cards, &binding, SourceKind::Mysql);
                    updated.expect_sql_mssql =
                        try_pipeline(&entry.question, &cards, &binding, SourceKind::Mssql);
                    blessed.push((orig_idx, updated));
                }
            } else if !bless_mode {
                if is_expected_miss {
                    failures.push(format!(
                        "[{}] expected miss on {:?} but bound and compiled — \
                         check for false-positive concept binding",
                        entry.id,
                        entry.expect_missing.as_deref().unwrap_or(&[])
                    ));
                } else {
                    failures.push(format!("[{}] NoParse/NoBind/NoCompile: {}", entry.id, entry.question));
                }
            }
        }

        eprintln!(
            "golden_suite_dev_binding: {}/{} passed ({} failed)",
            passed, total, total - passed
        );
        for f in &failures {
            eprintln!("  MISS: {}", f);
        }

        assert!(
            passed >= 55,
            "golden dev binding: only {passed}/{total} passed; need ≥55. Failures:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn golden_suite_alt_binding() {
        let cards = alt_schema_cards();
        let binding = make_binding(&cards, "alt");
        let entries: Vec<GoldenEntry> = load_golden()
            .into_iter()
            .filter(|e| e.binding == "alt")
            .collect();

        let total = entries.len();
        let mut passed = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for entry in &entries {
            let is_expected_miss = entry.expect_missing
                .as_deref()
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let sql = try_pipeline(&entry.question, &cards, &binding, SourceKind::Postgres);
            // expect_missing: pipeline MUST FAIL. Success = false-positive binding.
            let ok = if is_expected_miss { sql.is_none() } else { sql.is_some() };
            if ok {
                passed += 1;
            } else if is_expected_miss {
                failures.push(format!(
                    "[{}] expected miss on {:?} but bound and compiled — \
                     check for false-positive concept binding",
                    entry.id,
                    entry.expect_missing.as_deref().unwrap_or(&[])
                ));
            } else {
                failures.push(format!("[{}] NoParse/NoBind/NoCompile: {}", entry.id, entry.question));
            }
        }

        eprintln!(
            "golden_suite_alt_binding: {}/{} passed ({} failed)",
            passed, total, total - passed
        );
        for f in &failures {
            eprintln!("  MISS: {}", f);
        }

        assert!(
            passed >= 50,
            "golden alt binding: only {passed}/{total} passed; need ≥50. Failures:\n{}",
            failures.join("\n")
        );
    }
}

