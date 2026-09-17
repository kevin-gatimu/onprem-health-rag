//! Parses the real Postgres DDL (`docker/dev-postgres/init/01_schema.sql`) into
//! `TableCard`s and enum-value maps at test time, per plan 03e.
//!
//! Why: `dev_seed_cards()` used to be ~800 lines of hand-written `TableCard`s that
//! nothing reconciled against the schema, and 64% of blessed rows drifted from it
//! (`plans/new/03d-fixture-drift-audit.md`). This module removes the possibility of
//! drift by reading the same file the dev-postgres container boots from and deriving
//! the fixture from it — there is exactly one copy of the schema truth.
//!
//! Scope: this is not a general SQL parser. It handles only the constructs
//! `01_schema.sql` actually uses: `CREATE TYPE ... AS ENUM (...)`, `CREATE TABLE`
//! with column defs (`name`, `type`, `NOT NULL`, inline `PRIMARY KEY`, inline
//! `REFERENCES table(col)`, inline `CHECK (col IN (...))`), table-level `CHECK (col
//! IN (...))`, and the two `ALTER TABLE ... ADD CONSTRAINT ... FOREIGN KEY` statements
//! that attach forward-declared FKs. Anything else in the file (comments, `UNIQUE`,
//! multi-column constraints, `GENERATED ALWAYS AS (...) STORED`, `COMMENT ON TABLE`)
//! is either skipped or passed through opaquely — see the parser-limitations note at
//! the bottom of this file.

use std::collections::HashMap;

use crate::nl2sql::spec::{CardColumn, CardFkEdge, ColumnProfile, TableCard};

/// Path is relative to this file: `tests/` -> `ontology/` -> `src/` ->
/// `onprem-rag-server/` -> repo root -> `docker/...`. Verified with `ls` from
/// `src/ontology/tests/` before wiring this in (see plan 03e task notes).
const SCHEMA_SQL: &str = include_str!("../../../../docker/dev-postgres/init/01_schema.sql");

#[derive(Debug, Clone)]
pub(crate) struct ParsedColumn {
    pub name: String,
    pub type_: String,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_foreign_key: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedFk {
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ParsedTable {
    pub name: String,
    pub columns: Vec<ParsedColumn>,
    pub fk_edges: Vec<ParsedFk>,
    /// column name -> allowed labels, derived from `CREATE TYPE ... AS ENUM`
    /// (resolved through the column's declared type) and inline/table-level
    /// `CHECK (col IN (...))` lists.
    pub enum_values: HashMap<String, Vec<String>>,
}

pub(crate) struct ParsedSchema {
    pub tables: Vec<ParsedTable>,
    pub create_table_count: usize,
}

// ---------------------------------------------------------------------------
// Tokeniser helpers
// ---------------------------------------------------------------------------

/// Strips `--` line comments. Safe here because no string literal in this
/// schema file contains `--` (verified: the file has zero double-quoted
/// identifiers and no literal contains a hyphen pair).
fn strip_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits `text` on top-level occurrences of `sep`, respecting parenthesis
/// nesting and single-quoted string literals (`''` is an escaped quote).
fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            cur.push(c);
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    cur.push('\'');
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                cur.push(c);
            }
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            _ if c == sep && depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
        i += 1;
    }
    let tail = cur.trim();
    if !tail.is_empty() {
        parts.push(tail.to_string());
    }
    parts
}

/// Finds the first balanced parenthesised group at or after `from`, returning
/// its inner content (excluding the outer parens) and the index just past the
/// closing `)`.
fn extract_balanced(text: &str, from: usize) -> Option<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = from;
    while i < chars.len() && chars[i] != '(' {
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    let start = i + 1;
    let mut depth = 1i32;
    let mut in_string = false;
    i = start;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let inner: String = chars[start..i].iter().collect();
                    return Some((inner, i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('\'').unwrap_or(s);
    let s = s.strip_suffix('\'').unwrap_or(s);
    s.replace("''", "'")
}

/// Extracts `'a','b','c'`-style lists (used by both `ENUM (...)` and
/// `CHECK (col IN (...))`) into owned strings.
fn parse_quoted_list(inner: &str) -> Vec<String> {
    split_top_level(inner, ',')
        .into_iter()
        .map(|item| strip_quotes(&item))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Case-insensitive search for a keyword, returning a byte index valid in the
/// original (ASCII) string.
fn find_keyword(text: &str, kw: &str) -> Option<usize> {
    text.to_ascii_uppercase().find(kw)
}

/// Finds a `<col> IN (...)` clause inside `CHECK (...)` body content and
/// returns `(column_name, labels)`. Range checks (`BETWEEN`) and other
/// non-`IN` predicates are intentionally not matched.
fn find_check_in(check_body: &str) -> Option<(String, Vec<String>)> {
    let marker = " IN (";
    let pos = check_body.find(marker)?;
    let before = &check_body[..pos];
    let col: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if col.is_empty() {
        return None;
    }
    let (inner, _) = extract_balanced(check_body, pos)?;
    let labels = parse_quoted_list(&inner);
    if labels.is_empty() {
        return None;
    }
    Some((col, labels))
}

// ---------------------------------------------------------------------------
// CREATE TYPE ... AS ENUM
// ---------------------------------------------------------------------------

fn parse_enum_types(stmts: &[String]) -> HashMap<String, Vec<String>> {
    let mut types = HashMap::new();
    for stmt in stmts {
        let s = stmt.trim();
        if s.len() >= 11 && s[..11].eq_ignore_ascii_case("CREATE TYPE") {
            let rest = s[11..].trim();
            let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let name = rest[..name_end].to_string();
            if let Some(paren_pos) = rest.find('(') {
                if let Some((inner, _)) = extract_balanced(rest, paren_pos) {
                    types.insert(name, parse_quoted_list(&inner));
                }
            }
        }
    }
    types
}

// ---------------------------------------------------------------------------
// CREATE TABLE
// ---------------------------------------------------------------------------

fn parse_tables(stmts: &[String], enum_types: &HashMap<String, Vec<String>>) -> Vec<ParsedTable> {
    let mut tables = Vec::new();
    for stmt in stmts {
        let s = stmt.trim();
        if !(s.len() >= 12 && s[..12].eq_ignore_ascii_case("CREATE TABLE")) {
            continue;
        }
        let rest = s[12..].trim();
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        let name = rest[..name_end].trim().to_string();
        let Some(paren_pos) = rest.find('(') else {
            continue;
        };
        let Some((body, _)) = extract_balanced(rest, paren_pos) else {
            continue;
        };

        let mut table = ParsedTable {
            name: name.clone(),
            ..Default::default()
        };

        for item in split_top_level(&body, ',') {
            let it = item.trim();
            if it.is_empty() {
                continue;
            }
            let head: String = it
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            let head2: String = {
                let mut w = it.split_whitespace();
                let a = w.next().unwrap_or("");
                let b = w.next().unwrap_or("");
                format!("{} {}", a.to_ascii_uppercase(), b.to_ascii_uppercase())
            };

            if head == "CHECK" {
                if let Some(paren) = it.find('(') {
                    if let Some((inner, _)) = extract_balanced(it, paren) {
                        if let Some((col, labels)) = find_check_in(&inner) {
                            table.enum_values.entry(col).or_insert(labels);
                        }
                    }
                }
                continue;
            }
            if head == "UNIQUE" || head == "CONSTRAINT" || head2 == "PRIMARY KEY" || head2 == "FOREIGN KEY" {
                // Table-level constraints not tied to a single column's own
                // definition. This schema declares every PK inline on its
                // column and every FK either inline or via the two ALTER
                // TABLE statements handled by `apply_alter_fks` — see module
                // docs. UNIQUE/CONSTRAINT carry no information this fixture
                // needs.
                continue;
            }

            // Column definition: `<name> <type> <constraints...>`.
            let mut parts = it.splitn(2, char::is_whitespace);
            let col_name = parts.next().unwrap_or("").trim().to_string();
            if col_name.is_empty() {
                continue;
            }
            let remainder = parts.next().unwrap_or("").trim();
            let type_end = remainder.find(char::is_whitespace).unwrap_or(remainder.len());
            let raw_type = remainder[..type_end].to_string();
            let constraints = remainder[type_end..].trim();
            let constraints_upper = constraints.to_ascii_uppercase();

            let is_primary_key = constraints_upper.contains("PRIMARY KEY");
            let is_foreign_key = constraints_upper.contains("REFERENCES");
            let nullable = !constraints_upper.contains("NOT NULL") && !is_primary_key;

            // Column-level CHECK (col IN (...)).
            if let Some(check_pos) = find_keyword(constraints, "CHECK") {
                if let Some(paren_rel) = constraints[check_pos..].find('(') {
                    if let Some((inner, _)) =
                        extract_balanced(&constraints[check_pos..], paren_rel)
                    {
                        if let Some((_col, labels)) = find_check_in(&inner) {
                            table.enum_values.entry(col_name.clone()).or_insert(labels);
                        }
                    }
                }
            }

            // FK: REFERENCES <table>(<col>).
            if is_foreign_key {
                if let Some(ref_pos) = find_keyword(constraints, "REFERENCES") {
                    let after = constraints[ref_pos + "REFERENCES".len()..].trim_start();
                    let tbl_end = after
                        .find(|c: char| c.is_whitespace() || c == '(')
                        .unwrap_or(after.len());
                    let ref_table = after[..tbl_end].to_string();
                    if let Some(paren_rel) = after.find('(') {
                        if let Some((inner, _)) = extract_balanced(after, paren_rel) {
                            table.fk_edges.push(ParsedFk {
                                column: col_name.clone(),
                                ref_table,
                                ref_column: inner.trim().to_string(),
                            });
                        }
                    }
                }
            }

            // Resolve declared type: if it names a known ENUM type, record its
            // labels and mark the type string so `TypeClass::Text` keeps
            // matching it. Real Postgres reports enum columns as
            // `data_type = 'USER-DEFINED'`, which the classifier does not
            // special-case; `TypeClass::Text::matches` already treats the
            // literal substring "enum" as text, so we spell the type
            // `"<enum_type> enum"` to use that existing path rather than
            // inventing a new one. See module docs / task report for this
            // being a deliberate, general choice, not a per-row fix.
            let type_ = if let Some(labels) = enum_types.get(&raw_type) {
                table
                    .enum_values
                    .entry(col_name.clone())
                    .or_insert_with(|| labels.clone());
                format!("{raw_type} enum")
            } else {
                raw_type.to_ascii_lowercase()
            };

            table.columns.push(ParsedColumn {
                name: col_name,
                type_,
                nullable,
                is_primary_key,
                is_foreign_key,
            });
        }

        tables.push(table);
    }
    tables
}

/// Applies the two `ALTER TABLE ... ADD CONSTRAINT ... FOREIGN KEY (...)
/// REFERENCES ...` statements that attach forward-declared FKs
/// (`departments.head_provider_id`, `appointments.encounter_id`).
fn apply_alter_fks(tables: &mut [ParsedTable], stmts: &[String]) {
    for stmt in stmts {
        let s = stmt.trim();
        if !(s.len() >= 11 && s[..11].eq_ignore_ascii_case("ALTER TABLE")) {
            continue;
        }
        let rest = s[11..].trim();
        let tbl_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let table_name = rest[..tbl_end].to_string();

        let Some(fk_pos) = find_keyword(rest, "FOREIGN KEY") else {
            continue;
        };
        let after_fk = &rest[fk_pos..];
        let Some(paren) = after_fk.find('(') else {
            continue;
        };
        let Some((col, next_idx)) = extract_balanced(after_fk, paren) else {
            continue;
        };
        let col = col.trim().to_string();
        let after_col = &after_fk[next_idx..];
        let Some(ref_pos) = find_keyword(after_col, "REFERENCES") else {
            continue;
        };
        let after_ref = after_col[ref_pos + "REFERENCES".len()..].trim_start();
        let tbl_end2 = after_ref
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(after_ref.len());
        let ref_table = after_ref[..tbl_end2].to_string();
        let Some(paren2) = after_ref.find('(') else {
            continue;
        };
        let Some((ref_col, _)) = extract_balanced(after_ref, paren2) else {
            continue;
        };

        if let Some(t) = tables.iter_mut().find(|t| t.name == table_name) {
            t.fk_edges.push(ParsedFk {
                column: col.clone(),
                ref_table,
                ref_column: ref_col.trim().to_string(),
            });
            if let Some(c) = t.columns.iter_mut().find(|c| c.name == col) {
                c.is_foreign_key = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level parse + guards
// ---------------------------------------------------------------------------

pub(crate) fn parse_schema() -> ParsedSchema {
    let cleaned = strip_comments(SCHEMA_SQL);
    let stmts = split_top_level(&cleaned, ';');
    let create_table_count = stmts
        .iter()
        .filter(|s| {
            let t = s.trim();
            t.len() >= 12 && t[..12].eq_ignore_ascii_case("CREATE TABLE")
        })
        .count();
    let enum_types = parse_enum_types(&stmts);
    let mut tables = parse_tables(&stmts, &enum_types);
    apply_alter_fks(&mut tables, &stmts);
    ParsedSchema {
        tables,
        create_table_count,
    }
}

/// Guards against the parser silently producing a wrong fixture (plan 03e).
/// Called both from the canary tests and from every fixture-building entry
/// point, so a parser regression fails loudly wherever it is used, not just
/// when the canary test happens to run.
fn validate_schema(schema: &ParsedSchema) {
    assert_eq!(
        schema.tables.len(),
        schema.create_table_count,
        "DDL parser drift: parsed {} tables but 01_schema.sql has {} CREATE TABLE statements",
        schema.tables.len(),
        schema.create_table_count
    );
    for t in &schema.tables {
        assert!(
            !t.columns.is_empty(),
            "DDL parser drift: table {} parsed with zero columns",
            t.name
        );
        assert!(
            t.columns.iter().any(|c| c.is_primary_key),
            "DDL parser drift: table {} parsed with no primary key",
            t.name
        );
    }
}

// ---------------------------------------------------------------------------
// Fixture conversion
// ---------------------------------------------------------------------------

/// Builds the dev-seed `TableCard`s straight from `01_schema.sql`. Row counts
/// are not modelled by DDL, so every table gets the same nominal count; it
/// only ever affects enum-probe ordering in production (`binder.rs`) and
/// human-readable card text, never SQL shape or byte content.
pub(crate) fn dev_cards_from_ddl() -> Vec<TableCard> {
    let schema = parse_schema();
    validate_schema(&schema);
    schema
        .tables
        .iter()
        .map(|t| TableCard {
            source_id: "dev".to_string(),
            table_name: t.name.clone(),
            row_count: 1000,
            columns: t
                .columns
                .iter()
                .map(|c| CardColumn {
                    name: c.name.clone(),
                    type_: c.type_.clone(),
                    nullable: c.nullable,
                    is_primary_key: c.is_primary_key,
                    is_foreign_key: c.is_foreign_key,
                    sample_values: vec![],
                    profile: ColumnProfile::default(),
                })
                .collect(),
            fk_edges: t
                .fk_edges
                .iter()
                .map(|f| CardFkEdge {
                    column: f.column.clone(),
                    ref_table: f.ref_table.clone(),
                    ref_column: f.ref_column.clone(),
                })
                .collect(),
            card_vector: None,
            card_text: t.name.clone(),
        })
        .collect()
}

/// Ground-truth enum labels for the dev-seed schema, derived from the same
/// parse as `dev_cards_from_ddl` — table -> column -> labels. Feeds
/// `ColumnBinding.enum_values` via `bind_cards`'s `enum_values` parameter.
/// PII columns are not filtered out here: `build_column_bindings` already
/// zeroes `enum_values` for any column classified PII, regardless of what
/// this map contains, so this stays a faithful, unfiltered reflection of the
/// DDL and the PII guard is exercised for real rather than by omission.
pub(crate) fn dev_enum_values_from_ddl() -> HashMap<String, HashMap<String, Vec<String>>> {
    let schema = parse_schema();
    validate_schema(&schema);
    schema
        .tables
        .into_iter()
        .filter(|t| !t.enum_values.is_empty())
        .map(|t| (t.name, t.enum_values))
        .collect()
}

// ---------------------------------------------------------------------------
// Canary tests — guard the parser against silently producing a wrong fixture
// ---------------------------------------------------------------------------

#[cfg(test)]
mod parser_canaries {
    use super::*;

    #[test]
    fn parsed_table_count_matches_create_table_statements() {
        let schema = parse_schema();
        assert_eq!(schema.tables.len(), schema.create_table_count);
        // Sanity pin: if this fails because the schema legitimately grew or
        // shrank, update the constant — do not delete the assertion.
        assert_eq!(
            schema.tables.len(),
            64,
            "01_schema.sql should declare 64 CREATE TABLE statements; \
             if the schema intentionally changed, update this canary"
        );
    }

    #[test]
    fn every_table_has_columns_and_a_primary_key() {
        validate_schema(&parse_schema());
    }

    #[test]
    fn canary_beds_status_enum() {
        let schema = parse_schema();
        let beds = schema
            .tables
            .iter()
            .find(|t| t.name == "beds")
            .expect("beds table parsed");
        let mut labels = beds
            .enum_values
            .get("status")
            .expect("beds.status should carry bed_status enum labels")
            .clone();
        labels.sort();
        let mut expected: Vec<String> =
            ["available", "occupied", "cleaning", "maintenance", "blocked"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        expected.sort();
        assert_eq!(
            labels, expected,
            "beds.status should carry exactly the five bed_status labels"
        );
    }

    #[test]
    fn canary_lab_results_distinct_abnormality_columns() {
        let schema = parse_schema();
        let lab = schema
            .tables
            .iter()
            .find(|t| t.name == "lab_results")
            .expect("lab_results table parsed");
        for col in ["is_abnormal", "abnormal_flag", "is_critical"] {
            assert!(
                lab.columns.iter().any(|c| c.name == col),
                "lab_results should carry a distinct '{col}' column"
            );
        }
    }

    #[test]
    fn canary_admissions_has_no_invented_columns() {
        // plan 03e: admission_no / admission_status were hand-written into the
        // old fixture but do not exist in the real schema. A DDL-derived
        // fixture must not be able to resurrect them.
        let schema = parse_schema();
        let admissions = schema
            .tables
            .iter()
            .find(|t| t.name == "admissions")
            .expect("admissions table parsed");
        for absent in ["admission_no", "admission_status"] {
            assert!(
                !admissions.columns.iter().any(|c| c.name == absent),
                "admissions.{absent} does not exist in 01_schema.sql; a derived fixture cannot invent it"
            );
        }
    }

    #[test]
    fn canary_mortality_records_has_no_invented_columns() {
        let schema = parse_schema();
        let mortality = schema
            .tables
            .iter()
            .find(|t| t.name == "mortality_records")
            .expect("mortality_records table parsed");
        for absent in ["death_no", "national_id", "completed_at"] {
            assert!(
                !mortality.columns.iter().any(|c| c.name == absent),
                "mortality_records.{absent} does not exist in 01_schema.sql; a derived fixture cannot invent it"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Parser limitations (deliberately out of scope for this schema)
// ---------------------------------------------------------------------------
//
// - No support for schema-qualified or quoted identifiers (`"public"."foo"`).
//   01_schema.sql uses neither.
// - Table-level multi-column constraints (`UNIQUE (a, b)`, composite
//   `PRIMARY KEY (...)`, `FOREIGN KEY (...) REFERENCES ...` written inside a
//   `CREATE TABLE` body) are recognised and skipped, not modelled — this
//   schema has none of the latter two, only multi-column `UNIQUE`, which the
//   fixture has never needed.
// - `CHECK` clauses that do not contain an ` IN (` list (range checks such as
//   `BETWEEN`, cross-column checks) are parsed but intentionally yield no
//   enum values.
// - `GENERATED ALWAYS AS (...) STORED` expression bodies are treated as
//   opaque constraint text: they are scanned for `NOT NULL` / `REFERENCES` /
//   `CHECK` (matching none, correctly) but never interpreted.
