//! Schema-binding engine.
//!
//! # Two entry points
//!
//! - [`bind_cards`] — **sync, pure, dependency-free**.  Takes `TableCard` slices,
//!   optional pre-computed descriptor embeddings, and optional overrides; returns
//!   `Vec<TableBinding>`.  No async, no DB, fully unit-testable.
//!
//! - [`build_binding`] — **async wrapper** around `bind_cards`.  Probes enum values
//!   for categorical columns, resolves descriptor embeddings from AppState (computing
//!   and caching them on first call), then delegates the pure work to `bind_cards`.
//!
//! # Scoring formula
//!
//! With embeddings loaded:
//! - name_score  × 0.35
//! - column_score × 0.30
//! - role_score  × 0.15
//! - embed_score × 0.15
//! - fk_score    × 0.05
//!
//! When `embed::is_loaded()` is false the 0.15 embedding weight is dropped and the
//! remaining four weights are renormalised to sum to 1.0
//! (≈ 0.412 / 0.353 / 0.176 / 0.059).

use std::collections::{HashMap, HashSet, VecDeque};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::connectors::routes::is_likely_pii;
use crate::error::AppError;
use crate::nl2sql::spec::{CardFkEdge, TableCard};
use crate::ontology::binding::{ColumnBinding, JoinHop, SchemaBinding, TableBinding};
use crate::ontology::concepts::{
    descriptor, EntityConcept, ANCHOR_CONCEPTS, DESCRIPTORS,
};
use crate::ontology::roles::{ColumnRole, ROLE_TOKENS};
use crate::ontology::service_line::ServiceLine;

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

/// Manual overrides applied on top of the automatic scoring.
///
/// Set via `PUT /nl2sql/<source_id>/metadata`.  Tested by assertion 8 & 9.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BindingOverrides {
    /// Force a table to a specific concept (or `None` to remove the table from
    /// every service line).
    #[serde(default)]
    pub table_concepts: HashMap<String, Option<EntityConcept>>,
    /// Force column roles: outer key = table_name, inner key = column_name.
    #[serde(default)]
    pub column_roles: HashMap<String, HashMap<String, ColumnRole>>,
}

impl BindingOverrides {
    pub fn is_empty(&self) -> bool {
        self.table_concepts.is_empty() && self.column_roles.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Weight constants
// ---------------------------------------------------------------------------

const W_NAME: f32 = 0.35;
const W_COL: f32 = 0.30;
const W_ROLE: f32 = 0.15;
const W_EMB: f32 = 0.15;
const W_FK: f32 = 0.05;
const W_SUM_NO_EMB: f32 = W_NAME + W_COL + W_ROLE + W_FK; // 0.85

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Very simple English singularizer.  Handles the patterns seen in hospital
/// schema names without pulling in an NLP dependency.
fn singularize(word: &str) -> String {
    if word.ends_with("ies") && word.len() > 3 {
        format!("{}y", &word[..word.len() - 3])
    } else if word.ends_with("ses") && word.len() > 4 {
        // diagnoses → diagnosis, analyses → analysis
        format!("{}sis", &word[..word.len() - 3])
    } else if word.ends_with("xes") && word.len() > 3 {
        word[..word.len() - 2].to_string()
    } else if word.ends_with('s') && word.len() > 2 && !word.ends_with("ss") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

// ---------------------------------------------------------------------------
// Name scoring helpers (PascalCase + abbreviation normalization + containment)
// ---------------------------------------------------------------------------

/// Generic enterprise abbreviations used in healthcare IT schemas.
///
/// Entries are (abbreviation, expansion). Keep these generic — never add
/// dev-seed-specific or customer-specific names here.
const ABBREV_MAP: &[(&str, &str)] = &[
    ("req", "order"),
    ("res", "result"),
    ("rx", "prescription"),
    ("ob", "obstetric"),
    ("ipd", "inpatient"),
    ("opd", "outpatient"),
    ("dept", "department"),
    ("dob", "birth"),
    ("mrn", "identifier"),
    ("adm", "admission"),
    ("dt", "datetime"),
    ("amt", "amount"),
    ("usd", "amount"),
    ("diag", "diagnosis"),
    ("obs", "observation"),
    ("dx", "diagnosis"),
    ("hx", "history"),
    ("sx", "symptom"),
    ("tx", "treatment"),
];

/// Split a single PascalCase/CamelCase segment into lowercase parts.
///
/// Splits at:
/// - uppercase following a lowercase letter (`"Patient|M"` in `"PatientMaster"`)
/// - uppercase starting a TitleCase word after an all-upper acronym run
///   (`"IPD|A"` in `"IPDAdmission"`)
///
/// Examples: `"PatientMaster"` → `["patient", "master"]`,
/// `"IPDAdmission"` → `["ipd", "admission"]`,
/// `"RxLine"` → `["rx", "line"]`.
fn pascal_split_segment(s: &str) -> Vec<String> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n == 0 {
        return vec![];
    }
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for i in 0..n {
        let c = bytes[i] as char;
        if c.is_ascii_uppercase() && !cur.is_empty() {
            let prev_lower = bytes[i - 1].is_ascii_lowercase();
            let next_lower = i + 1 < n && bytes[i + 1].is_ascii_lowercase();
            // Split at lowercase→upper boundary OR at the last char of an
            // all-uppercase acronym run before a TitleCase word begins.
            let is_acronym_end = !prev_lower
                && next_lower
                && cur.bytes().all(|b| (b as char).is_ascii_uppercase() || (b as char).is_ascii_digit());
            if prev_lower || is_acronym_end {
                parts.push(cur.to_ascii_lowercase());
                cur = String::new();
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        parts.push(cur.to_ascii_lowercase());
    }
    parts
}

/// Expand a table name into a normalized token set for containment scoring.
///
/// Algorithm:
/// 1. Split on `_` and space.
/// 2. PascalCase-split each segment.
/// 3. Lowercase + singularize each sub-token.
/// 4. Also add the abbreviation expansion for any token in `ABBREV_MAP`.
fn expand_table_tokens(name: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for seg in name.split(|c: char| c == '_' || c == ' ').filter(|s| !s.is_empty()) {
        for raw in pascal_split_segment(seg) {
            let low = raw.to_ascii_lowercase();
            let sing = singularize(&low);
            set.insert(low.clone());
            if sing != low {
                set.insert(sing.clone());
            }
            for &(abbrev, expanded) in ABBREV_MAP {
                if low == abbrev || sing == abbrev {
                    set.insert(expanded.to_string());
                    break;
                }
            }
        }
    }
    set
}

/// Expand a descriptor `name_token` (potentially underscore-joined) into its
/// constituent parts.  `"lab_order"` → `["lab", "order"]`.
fn expand_desc_token(token: &str) -> Vec<String> {
    token
        .split('_')
        .map(|seg| seg.to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Fraction of `desc_parts` that appear in `table_toks`.
///
/// `|desc_parts ∩ table_toks| / |desc_parts|`
fn containment(desc_parts: &[String], table_toks: &HashSet<String>) -> f32 {
    if desc_parts.is_empty() {
        return 0.0;
    }
    let matched = desc_parts
        .iter()
        .filter(|t| table_toks.contains(t.as_str()))
        .count();
    matched as f32 / desc_parts.len() as f32
}

/// Name-match score for a table against a concept descriptor.
///
/// Priority:
/// 1. Exact whole-name match (raw or underscore-stripped) in `name_tokens` → 1.0
/// 2. Same match in `synonyms` → 0.95
/// 3. Max containment over each descriptor token used as its own alternative
///    set against the expanded table tokens (PascalCase + abbreviation aware)
///    — synonyms discounted to 0.9.
fn name_score(table_name: &str, desc: &crate::ontology::concepts::ConceptDescriptor) -> f32 {
    let low = table_name.to_ascii_lowercase();
    let sing = singularize(&low);
    let strip_us = |s: &str| -> String { s.replace('_', "") };

    // 1. Exact whole-name match — including underscore-stripped variants so
    //    e.g. "PatientMaster" matches descriptor token "patient_master".
    if desc.name_tokens.iter().any(|&t| {
        t == low
            || t == sing.as_str()
            || strip_us(t) == low
            || strip_us(t) == sing.as_str()
    }) {
        return 1.0;
    }
    // 2. Synonym exact match (same underscore-stripped logic)
    if desc.synonyms.iter().any(|&s| {
        s == low
            || s == sing.as_str()
            || strip_us(s) == low
            || strip_us(s) == sing.as_str()
    }) {
        return 0.95;
    }

    // 3. Containment scoring: for each descriptor token, treat its parts as an
    //    independent alternative set and compute containment in the expanded
    //    table token set.
    let table_toks = expand_table_tokens(table_name);

    let best_name = desc
        .name_tokens
        .iter()
        .map(|&t| containment(&expand_desc_token(t), &table_toks))
        .fold(0.0_f32, f32::max);

    let best_syn = desc
        .synonyms
        .iter()
        .map(|&s| containment(&expand_desc_token(s), &table_toks))
        .fold(0.0_f32, f32::max)
        * 0.9;

    best_name.max(best_syn)
}

/// Column-coverage score: fraction of descriptor column_tokens found (as
/// case-insensitive substring) in the table's column names.
fn column_score(
    columns: &[crate::nl2sql::spec::CardColumn],
    desc: &crate::ontology::concepts::ConceptDescriptor,
) -> f32 {
    if desc.column_tokens.is_empty() {
        return 0.5; // neutral
    }
    let col_names: Vec<String> = columns
        .iter()
        .map(|c| c.name.to_ascii_lowercase())
        .collect();
    let present = desc
        .column_tokens
        .iter()
        .filter(|&&tok| col_names.iter().any(|cn| cn.contains(tok)))
        .count();
    present as f32 / desc.column_tokens.len() as f32
}

/// Preliminary column role (before FK concept refinement).
///
/// Precedence:
/// 1. PrimaryKey / ForeignRef from structural flags
/// 2. First-match in ROLE_TOKENS (name + type_class)
/// 3. Unknown
fn preliminary_role(col: &crate::nl2sql::spec::CardColumn) -> ColumnRole {
    // Structural flags take precedence
    if col.is_primary_key {
        return ColumnRole::PrimaryKey;
    }
    let col_name_low = col.name.to_ascii_lowercase();
    let type_low = col.type_.to_ascii_lowercase();
    for rt in ROLE_TOKENS {
        let type_ok = rt
            .type_class
            .map(|tc| tc.matches(&type_low))
            .unwrap_or(true);
        if !type_ok {
            continue;
        }
        if rt.tokens.iter().any(|&tok| col_name_low.contains(tok)) {
            return rt.role;
        }
    }
    if col.is_foreign_key {
        return ColumnRole::ForeignRef;
    }
    ColumnRole::Unknown
}

/// Role-coverage score: fraction of concept's required_roles that appear among
/// the table's preliminary column roles.
fn role_score(
    prelim_roles: &[ColumnRole],
    desc: &crate::ontology::concepts::ConceptDescriptor,
) -> f32 {
    if desc.required_roles.is_empty() {
        return 0.5; // neutral
    }
    let role_set: HashSet<ColumnRole> = prelim_roles.iter().copied().collect();
    let matched = desc
        .required_roles
        .iter()
        .filter(|&&r| role_set.contains(&r))
        .count();
    matched as f32 / desc.required_roles.len() as f32
}

/// FK-shape heuristic score.
///
/// Concepts that expect FK columns (have PatientRef, ProviderRef, etc. in
/// required_roles) get a boost when the table has FK columns with matching
/// name patterns.  Hub concepts (Patient, Provider, Department) get a boost
/// when the table has NO outgoing FK columns that would indicate it's a
/// transaction table.
fn fk_score(
    columns: &[crate::nl2sql::spec::CardColumn],
    desc: &crate::ontology::concepts::ConceptDescriptor,
    concept: EntityConcept,
) -> f32 {
    // Hub concepts: Patient, Provider, Department, Medication, Ward, Bed should have few/no FK cols
    let is_hub = matches!(
        concept,
        EntityConcept::Patient
            | EntityConcept::Provider
            | EntityConcept::Department
            | EntityConcept::Medication
            | EntityConcept::Ward
            | EntityConcept::Bed
            | EntityConcept::DiagnosisCode
            | EntityConcept::LabTest
            | EntityConcept::ProcedureCode
            | EntityConcept::Allergen
            | EntityConcept::CareProgram
            | EntityConcept::Vaccine
            | EntityConcept::Equipment
            | EntityConcept::Insurer
    );
    let fk_count = columns.iter().filter(|c| c.is_foreign_key).count();
    let total = columns.len().max(1);

    if is_hub {
        // Prefer tables with low FK ratio for hub concepts
        let fk_ratio = fk_count as f32 / total as f32;
        return (1.0 - fk_ratio * 0.5).clamp(0.0, 1.0);
    }

    // For edge concepts: check if required FK roles have matching column names
    let ref_roles: Vec<ColumnRole> = desc
        .required_roles
        .iter()
        .filter(|&&r| is_ref_role(r))
        .copied()
        .collect();

    if ref_roles.is_empty() {
        return 0.5; // neutral
    }

    let fk_cols: Vec<String> = columns
        .iter()
        .filter(|c| c.is_foreign_key)
        .map(|c| c.name.to_ascii_lowercase())
        .collect();

    let matched = ref_roles
        .iter()
        .filter(|&&r| fk_cols.iter().any(|cn| col_matches_ref_role(cn, r)))
        .count();

    (matched as f32 / ref_roles.len() as f32).max(0.1)
}

fn is_ref_role(r: ColumnRole) -> bool {
    matches!(
        r,
        ColumnRole::PatientRef
            | ColumnRole::EncounterRef
            | ColumnRole::ProviderRef
            | ColumnRole::DepartmentRef
            | ColumnRole::WardRef
            | ColumnRole::BedRef
            | ColumnRole::ForeignRef
    )
}

fn col_matches_ref_role(col_name: &str, role: ColumnRole) -> bool {
    match role {
        ColumnRole::PatientRef => {
            col_name.contains("patient") || col_name.contains("person")
        }
        ColumnRole::EncounterRef => col_name.contains("encounter") || col_name.contains("visit"),
        ColumnRole::ProviderRef => {
            col_name.contains("provider") || col_name.contains("staff") || col_name.contains("doctor")
        }
        ColumnRole::DepartmentRef => col_name.contains("department") || col_name.contains("dept"),
        ColumnRole::WardRef => col_name.contains("ward"),
        ColumnRole::BedRef => col_name.contains("bed"),
        ColumnRole::ForeignRef => col_name.ends_with("_id"),
        _ => false,
    }
}

/// Cosine similarity between two vectors.  Returns 0.5 (neutral) if either is
/// empty or zero-magnitude.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.5;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.5;
    }
    (dot / (mag_a * mag_b)).clamp(-1.0, 1.0) * 0.5 + 0.5 // map [-1,1] → [0,1]
}

// ---------------------------------------------------------------------------
// Core pure-score function
// ---------------------------------------------------------------------------

struct ConceptScore {
    concept: EntityConcept,
    score: f32,
}

/// Compute the weighted score for one (table, concept) pair.
fn score_table_concept(
    card: &TableCard,
    prelim_roles: &[ColumnRole],
    concept: EntityConcept,
    desc: &crate::ontology::concepts::ConceptDescriptor,
    descriptor_vectors: Option<&HashMap<EntityConcept, Vec<f32>>>,
) -> f32 {
    let ns = name_score(&card.table_name, desc);
    let cs = column_score(&card.columns, desc);
    let rs = role_score(prelim_roles, desc);
    let fs = fk_score(&card.columns, desc, concept);

    let embed_vec = descriptor_vectors.and_then(|dv| dv.get(&concept));
    let card_vec = card.card_vector.as_deref();

    match (card_vec, embed_vec) {
        (Some(cv), Some(ev)) => {
            let es = cosine_similarity(cv, ev);
            ns * W_NAME + cs * W_COL + rs * W_ROLE + es * W_EMB + fs * W_FK
        }
        _ => {
            // Degraded: renormalise remaining weights
            (ns * W_NAME + cs * W_COL + rs * W_ROLE + fs * W_FK) / W_SUM_NO_EMB
        }
    }
}

// ---------------------------------------------------------------------------
// BFS patient path
// ---------------------------------------------------------------------------

/// Build BFS patient paths for all tables.  The patient table is identified by
/// the table in `assignments` with concept == `EntityConcept::Patient`.
///
/// Returns a map of table_name → patient path (same semantics as
/// `TableBinding::patient_path`).
fn compute_patient_paths(
    cards: &[TableCard],
    assignments: &HashMap<String, EntityConcept>,
    max_hops: usize,
) -> HashMap<String, Option<Vec<JoinHop>>> {
    // Find the patient table name
    let patient_table: Option<&str> = assignments
        .iter()
        .find_map(|(t, &c)| if c == EntityConcept::Patient { Some(t.as_str()) } else { None });

    let Some(patient_table) = patient_table else {
        // No patient table — all paths unknown
        return cards
            .iter()
            .map(|c| (c.table_name.clone(), None))
            .collect();
    };

    // Build FK adjacency: table → list of (to_table, join_col, via_col)
    let mut adj: HashMap<&str, Vec<(&str, &str, &str)>> = HashMap::new();
    for card in cards {
        let e = adj.entry(card.table_name.as_str()).or_default();
        for fk in &card.fk_edges {
            e.push((
                fk.ref_table.as_str(),
                fk.column.as_str(),
                fk.ref_column.as_str(),
            ));
        }
    }

    let mut result: HashMap<String, Option<Vec<JoinHop>>> = HashMap::new();

    for card in cards {
        let tname = card.table_name.as_str();
        if tname == patient_table {
            result.insert(card.table_name.clone(), Some(vec![]));
            continue;
        }

        // BFS
        let mut visited: HashSet<&str> = HashSet::new();
        visited.insert(tname);
        // Queue: (current_table, path_so_far)
        let mut queue: VecDeque<(&str, Vec<JoinHop>)> = VecDeque::new();
        queue.push_back((tname, vec![]));
        let mut found: Option<Vec<JoinHop>> = None;

        'bfs: while let Some((current, path)) = queue.pop_front() {
            if path.len() >= max_hops {
                continue;
            }
            if let Some(neighbours) = adj.get(current) {
                for &(to, join_col, via_col) in neighbours {
                    if visited.contains(to) {
                        continue;
                    }
                    let mut new_path = path.clone();
                    new_path.push(JoinHop {
                        from_table: current.to_string(),
                        join_col: join_col.to_string(),
                        to_table: to.to_string(),
                        via_col: via_col.to_string(),
                    });
                    if to == patient_table {
                        found = Some(new_path);
                        break 'bfs;
                    }
                    visited.insert(to);
                    queue.push_back((to, new_path));
                }
            }
        }

        result.insert(card.table_name.clone(), found);
    }

    result
}

// ---------------------------------------------------------------------------
// EventTime selection
// ---------------------------------------------------------------------------

/// Select the single `EventTime` column for a table.
///
/// Rule: prefer domain-named EventTime columns (anything that is NOT
/// `created_at`, `updated_at`, `modified_at`, `created_on`, `updated_on`).
/// Fall back to any EventTime column.  `StartTime`/`EndTime` are acceptable
/// when no EventTime is found.
///
/// NOTE (defect-closure sweep, see nl2sql/ir/bind.rs): this function still
/// picks the first declared column when more than one domain EventTime
/// candidate exists (e.g. `incident_reports.occurred_at` vs `.reported_at`),
/// which is the same "position, not meaning" defect class fixed on the LIVE
/// column-selection path in `nl2sql::ir::bind::column_with_role_hint_in_table`.
/// It was deliberately NOT changed here: `event_time_col` is informational
/// only (exposed via the `/ontology` schema route; the query pipeline
/// resolves its own EventTime column independently in `nl2sql::ir::bind` and
/// never reads this field), and `ontology::tests::exactly_one_event_time_per_bound_table`
/// asserts every table with EventTime columns gets a non-`None` value here —
/// an assertion this task has no mandate to relax. Left as a flagged,
/// unfixed site rather than silently worked around; see the sweep report.
fn select_event_time(columns: &[ColumnBinding]) -> Option<String> {
    let generic_names = [
        "created_at", "updated_at", "modified_at", "created_on",
        "updated_on", "deleted_at", "created_date", "modified_date",
    ];
    // Prefer domain EventTime (not a generic timestamp)
    let domain: Option<&ColumnBinding> = columns.iter().find(|c| {
        c.role == ColumnRole::EventTime
            && !generic_names.iter().any(|&g| c.column_name == g)
    });
    if let Some(d) = domain {
        return Some(d.column_name.clone());
    }
    // Fall back to any EventTime
    let any_et = columns.iter().find(|c| c.role == ColumnRole::EventTime);
    if let Some(et) = any_et {
        return Some(et.column_name.clone());
    }
    // Last resort: StartTime
    columns
        .iter()
        .find(|c| c.role == ColumnRole::StartTime)
        .map(|c| c.column_name.clone())
}

// ---------------------------------------------------------------------------
// Column binding assembly
// ---------------------------------------------------------------------------

fn build_column_bindings(
    card: &TableCard,
    col_overrides: Option<&HashMap<String, ColumnRole>>,
    enum_values: &HashMap<String, Vec<String>>,
) -> Vec<ColumnBinding> {
    card.columns
        .iter()
        .map(|col| {
            let pii = is_likely_pii(&col.name)
                || preliminary_role(col).is_pii();
            let role = col_overrides
                .and_then(|ov| ov.get(&col.name))
                .copied()
                .unwrap_or_else(|| preliminary_role(col));
            let ev = if pii {
                vec![]
            } else {
                enum_values.get(&col.name).cloned().unwrap_or_default()
            };
            ColumnBinding {
                column_name: col.name.clone(),
                role,
                is_pii: pii,
                enum_values: ev,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Refine FK-based roles
// ---------------------------------------------------------------------------

/// After concept assignments are known, update `ColumnRole::ForeignRef` for
/// FK columns that point to a table with a known concept.
fn refine_fk_roles(
    columns: &mut Vec<ColumnBinding>,
    fk_edges: &[CardFkEdge],
    assignments: &HashMap<String, EntityConcept>,
) {
    for fk in fk_edges {
        // Find the column binding for this FK
        let Some(cb) = columns.iter_mut().find(|c| c.column_name == fk.column) else {
            continue;
        };
        // Only refine if currently ForeignRef or Unknown
        if !matches!(cb.role, ColumnRole::ForeignRef | ColumnRole::Unknown) {
            continue;
        }
        let Some(&ref_concept) = assignments.get(&fk.ref_table) else {
            continue;
        };
        let refined = match ref_concept {
            EntityConcept::Patient => ColumnRole::PatientRef,
            EntityConcept::Provider => ColumnRole::ProviderRef,
            EntityConcept::Encounter => ColumnRole::EncounterRef,
            EntityConcept::Department => ColumnRole::DepartmentRef,
            EntityConcept::Ward => ColumnRole::WardRef,
            EntityConcept::Bed => ColumnRole::BedRef,
            _ => continue,
        };
        cb.role = refined;
    }
}

// ---------------------------------------------------------------------------
// Service-line assignment
// ---------------------------------------------------------------------------

fn service_lines_for(concept: EntityConcept) -> Vec<ServiceLine> {
    ServiceLine::owner_of(concept).to_vec()
}

// ---------------------------------------------------------------------------
// Tie-breaking helper
// ---------------------------------------------------------------------------

/// Count how many tables in the current (partial) assignment belong to each
/// service line.
fn line_counts(assignments: &HashMap<String, EntityConcept>) -> HashMap<ServiceLine, usize> {
    let mut counts: HashMap<ServiceLine, usize> = HashMap::new();
    for &c in assignments.values() {
        for &line in ServiceLine::owner_of(c) {
            *counts.entry(line).or_insert(0) += 1;
        }
    }
    counts
}

/// When two concepts tie, prefer the one whose owning service lines are
/// currently underrepresented in the assignment.
fn tiebreak_concept(
    candidates: &[ConceptScore],
    line_counts: &HashMap<ServiceLine, usize>,
) -> EntityConcept {
    candidates
        .iter()
        .min_by(|a, b| {
            let a_min = service_lines_for(a.concept)
                .iter()
                .map(|l| line_counts.get(l).copied().unwrap_or(0))
                .min()
                .unwrap_or(0);
            let b_min = service_lines_for(b.concept)
                .iter()
                .map(|l| line_counts.get(l).copied().unwrap_or(0))
                .min()
                .unwrap_or(0);
            a_min.cmp(&b_min)
        })
        .map(|cs| cs.concept)
        .unwrap_or(EntityConcept::Unknown)
}

// ---------------------------------------------------------------------------
// bind_cards — the public pure entry point
// ---------------------------------------------------------------------------

/// Bind a slice of `TableCard`s to their semantic concepts.
///
/// This function is **sync, pure, and dependency-free**: no async, no DB,
/// no network.  It is suitable for unit testing with fixture data.
///
/// `descriptor_vectors`: pre-computed BGE-M3 embeddings for each concept's
/// description.  When `None` the embedding weight (0.15) is dropped and the
/// remaining weights are renormalised.
pub fn bind_cards(
    cards: &[TableCard],
    min_confidence: f32,
    max_hops: usize,
    descriptor_vectors: Option<&HashMap<EntityConcept, Vec<f32>>>,
    overrides: Option<&BindingOverrides>,
    enum_values: &HashMap<String, HashMap<String, Vec<String>>>,
) -> Vec<TableBinding> {
    if cards.is_empty() {
        return vec![];
    }

    let all_descriptors: Vec<(EntityConcept, &crate::ontology::concepts::ConceptDescriptor)> =
        DESCRIPTORS
            .iter()
            .map(|d| (d.concept, d))
            .collect();

    // Pre-compute preliminary roles for each table
    let prelim_roles_map: HashMap<&str, Vec<ColumnRole>> = cards
        .iter()
        .map(|card| {
            let roles: Vec<ColumnRole> = card.columns.iter().map(preliminary_role).collect();
            (card.table_name.as_str(), roles)
        })
        .collect();

    // Forced-concept tables from overrides
    let forced: HashMap<&str, Option<EntityConcept>> = overrides
        .map(|ov| {
            ov.table_concepts
                .iter()
                .map(|(t, c)| (t.as_str(), *c))
                .collect()
        })
        .unwrap_or_default();

    // -----------------------------------------------------------------------
    // Pass 1: Anchor concepts
    // -----------------------------------------------------------------------
    let mut assignments: HashMap<String, EntityConcept> = HashMap::new();
    let mut assigned_tables: HashSet<String> = HashSet::new();

    // Apply forced concepts first
    for card in cards {
        if let Some(forced_concept) = forced.get(card.table_name.as_str()) {
            match forced_concept {
                Some(c) => {
                    assignments.insert(card.table_name.clone(), *c);
                    assigned_tables.insert(card.table_name.clone());
                }
                None => {
                    // concept: None → mark as assigned to Unknown so it doesn't compete
                    assignments.insert(card.table_name.clone(), EntityConcept::Unknown);
                    assigned_tables.insert(card.table_name.clone());
                }
            }
        }
    }

    for &anchor in ANCHOR_CONCEPTS {
        if assigned_tables.iter().any(|t| assignments.get(t) == Some(&anchor)) {
            continue; // already assigned via override
        }
        let desc = descriptor(anchor);
        let best = cards
            .iter()
            .filter(|c| !assigned_tables.contains(&c.table_name))
            .filter_map(|card| {
                let roles = prelim_roles_map.get(card.table_name.as_str()).map(|v| v.as_slice()).unwrap_or(&[]);
                let s = score_table_concept(card, roles, anchor, desc, descriptor_vectors);
                if s >= min_confidence { Some((card, s)) } else { None }
            })
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((card, _)) = best {
            assignments.insert(card.table_name.clone(), anchor);
            assigned_tables.insert(card.table_name.clone());
        }
    }

    // -----------------------------------------------------------------------
    // Pass 2: All remaining tables
    // -----------------------------------------------------------------------
    for card in cards {
        if assigned_tables.contains(&card.table_name) {
            continue;
        }
        let prelim = prelim_roles_map
            .get(card.table_name.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let lc = line_counts(&assignments);

        let mut scored: Vec<ConceptScore> = all_descriptors
            .iter()
            .map(|&(concept, desc)| {
                let s = score_table_concept(card, prelim, concept, desc, descriptor_vectors);
                ConceptScore { concept, score: s }
            })
            .collect();
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        let best_score = scored.first().map(|c| c.score).unwrap_or(0.0);

        if best_score < min_confidence {
            assignments.insert(card.table_name.clone(), EntityConcept::Unknown);
        } else {
            // Collect all candidates within 2% of best score for tie-breaking
            let threshold = best_score - 0.02;
            let candidates: Vec<ConceptScore> = scored
                .into_iter()
                .filter(|cs| {
                    cs.score >= threshold
                        // Don't reassign a concept already taken by another table
                        && !assignments.values().any(|&a| a == cs.concept)
                })
                .take(5)
                .collect();

            let chosen = if candidates.is_empty() {
                EntityConcept::Unknown
            } else if candidates.len() == 1 {
                candidates[0].concept
            } else {
                tiebreak_concept(&candidates, &lc)
            };
            assignments.insert(card.table_name.clone(), chosen);
        }
        assigned_tables.insert(card.table_name.clone());
    }

    // -----------------------------------------------------------------------
    // Compute confidence scores
    // -----------------------------------------------------------------------
    let confidences: HashMap<String, f32> = cards
        .iter()
        .map(|card| {
            let concept = assignments.get(&card.table_name).copied().unwrap_or(EntityConcept::Unknown);
            if concept == EntityConcept::Unknown {
                return (card.table_name.clone(), 0.0);
            }
            let desc = descriptor(concept);
            let prelim = prelim_roles_map
                .get(card.table_name.as_str())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let s = score_table_concept(card, prelim, concept, desc, descriptor_vectors);
            (card.table_name.clone(), s)
        })
        .collect();

    // -----------------------------------------------------------------------
    // Compute patient paths
    // -----------------------------------------------------------------------
    let patient_paths = compute_patient_paths(cards, &assignments, max_hops);

    // -----------------------------------------------------------------------
    // Assemble TableBindings
    // -----------------------------------------------------------------------
    let mut bindings: Vec<TableBinding> = cards
        .iter()
        .map(|card| {
            let concept = assignments.get(&card.table_name).copied().unwrap_or(EntityConcept::Unknown);
            let confidence = *confidences.get(&card.table_name).unwrap_or(&0.0);
            let svc_lines = service_lines_for(concept);

            let col_overrides = overrides
                .and_then(|ov| ov.column_roles.get(&card.table_name));
            let ev_map = enum_values.get(&card.table_name).cloned().unwrap_or_default();
            let mut cols = build_column_bindings(card, col_overrides, &ev_map);

            // Refine FK-based roles now that assignments are known
            refine_fk_roles(&mut cols, &card.fk_edges, &assignments);

            let event_time = select_event_time(&cols);
            let path = patient_paths.get(&card.table_name).cloned().flatten();
            let patient_path = if card.table_name == assignments.iter().find_map(|(t, &c)| if c == EntityConcept::Patient { Some(t.as_str()) } else { None }).unwrap_or("") {
                Some(vec![])
            } else {
                patient_paths.get(&card.table_name).cloned().unwrap_or(None)
            };
            let _ = path; // consumed above

            let degraded = descriptor_vectors.is_none();

            TableBinding {
                table_name: card.table_name.clone(),
                concept,
                confidence,
                service_lines: svc_lines,
                columns: cols,
                patient_path,
                event_time_col: event_time,
                degraded,
            }
        })
        .collect();

    // Sort by table name for determinism (assertion 10)
    bindings.sort_by(|a, b| a.table_name.cmp(&b.table_name));
    bindings
}

// ---------------------------------------------------------------------------
// build_binding — async wrapper with enum probes
// ---------------------------------------------------------------------------

/// Build (or rebuild) the `SchemaBinding` for a source.
///
/// This function:
/// 1. Resolves descriptor embeddings from `AppState`, computing and caching
///    them on first call (degraded if fastembed not loaded).
/// 2. Probes `SELECT DISTINCT <col> … LIMIT 26` for categorical columns,
///    capped at 40 probes per build, ordered by descending table `row_count`.
/// 3. Calls the pure `bind_cards` function.
/// 4. Returns a `SchemaBinding` ready for persistence.
pub async fn build_binding(
    cards: &[TableCard],
    db: &crate::documentdb::DocumentDb,
    config: &crate::config::Config,
    source_id: &str,
    descriptor_vectors: Option<&HashMap<EntityConcept, Vec<f32>>>,
    overrides: Option<&BindingOverrides>,
) -> Result<SchemaBinding, AppError> {
    let degraded = descriptor_vectors.is_none();

    // -----------------------------------------------------------------------
    // Enum probes (up to 40, ordered by descending row_count)
    // -----------------------------------------------------------------------
    let enum_values = probe_enum_values(cards, db, config, source_id).await;

    // -----------------------------------------------------------------------
    // Bind
    // -----------------------------------------------------------------------
    let min_conf = config.binding_min_confidence;
    let max_hops = config.binding_max_hops;

    let tables = bind_cards(cards, min_conf, max_hops, descriptor_vectors, overrides, &enum_values);

    Ok(SchemaBinding {
        source_id: source_id.to_string(),
        bound_at: Utc::now(),
        tables,
        degraded,
        override_version: 0,
    })
}

/// Probe `SELECT DISTINCT <col> FROM <table> LIMIT 26` for categorical columns.
///
/// Capped at `ENUM_PROBE_LIMIT` probes per build, prioritised by descending
/// `row_count`.  PII columns are never probed.
///
/// Returns a map of table_name → {column_name → values}.  Never fails — errors
/// per-column are logged and skipped.
async fn probe_enum_values(
    cards: &[TableCard],
    db: &crate::documentdb::DocumentDb,
    config: &crate::config::Config,
    source_id: &str,
) -> HashMap<String, HashMap<String, Vec<String>>> {
    const ENUM_PROBE_LIMIT: usize = 40;
    const ENUM_DISTINCT_LIMIT: usize = 26;

    // Collect probe targets: (row_count, table_name, col_name)
    let mut targets: Vec<(i64, String, String)> = vec![];
    for card in cards {
        for col in &card.columns {
            let pii = is_likely_pii(&col.name);
            if pii {
                continue;
            }
            let prelim = preliminary_role(col);
            if !prelim.is_categorical() {
                continue;
            }
            targets.push((card.row_count, card.table_name.clone(), col.name.clone()));
        }
    }
    // Sort descending by row_count
    targets.sort_by(|a, b| b.0.cmp(&a.0));
    targets.truncate(ENUM_PROBE_LIMIT);

    let mut out: HashMap<String, HashMap<String, Vec<String>>> = HashMap::new();

    for (_, table_name, col_name) in targets {
        let sql = format!(
            "SELECT DISTINCT {col_name} FROM {table_name} WHERE {col_name} IS NOT NULL LIMIT {ENUM_DISTINCT_LIMIT}"
        );

        // Validate SQL (non-fatal)
        let dialect = match get_source_kind(db, config, source_id).await {
            Some(k) => k,
            None => break,
        };
        let allowed = vec![table_name.clone()];
        if crate::nl2sql::validate::validate_sql(&sql, dialect, ENUM_DISTINCT_LIMIT as i64, &allowed).is_err() {
            continue;
        }

        match crate::nl2sql::execute::run_select(db, config, source_id, &sql).await {
            Ok((_cols, rows)) => {
                let values: Vec<String> = rows
                    .into_iter()
                    .filter_map(|row| row.into_iter().next())
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        serde_json::Value::Bool(b) => Some(b.to_string()),
                        _ => None,
                    })
                    .take(25)
                    .collect();
                if !values.is_empty() {
                    out.entry(table_name)
                        .or_default()
                        .insert(col_name, values);
                }
            }
            Err(e) => {
                tracing::debug!("enum probe skipped for {table_name}.{col_name}: {e}");
            }
        }
    }

    out
}

async fn get_source_kind(
    db: &crate::documentdb::DocumentDb,
    config: &crate::config::Config,
    source_id: &str,
) -> Option<crate::connectors::SourceKind> {
    crate::connectors::routes::load_spec(db, config, source_id)
        .await
        .ok()
        .map(|spec| spec.kind)
}

// ---------------------------------------------------------------------------
// Compute descriptor embeddings (called by build_binding or state init)
// ---------------------------------------------------------------------------

/// Compute BGE-M3 embeddings for all non-Unknown entity concept descriptions.
/// Returns `None` if fastembed is not loaded.
pub async fn compute_descriptor_vectors(
    config: &crate::config::Config,
) -> Option<HashMap<EntityConcept, Vec<f32>>> {
    if !crate::embed::is_loaded() {
        return None;
    }
    let concepts_and_texts: Vec<(EntityConcept, String)> = DESCRIPTORS
        .iter()
        .map(|d| (d.concept, d.description.to_string()))
        .collect();

    let texts: Vec<String> = concepts_and_texts.iter().map(|(_, t)| t.clone()).collect();
    match crate::embed::embed_documents(config, texts).await {
        Ok(vecs) => {
            let map: HashMap<EntityConcept, Vec<f32>> = concepts_and_texts
                .into_iter()
                .zip(vecs)
                .map(|((c, _), v)| (c, v))
                .collect();
            Some(map)
        }
        Err(e) => {
            tracing::warn!("descriptor embedding failed, running in degraded mode: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Build a minimal TableCard for testing.
    pub fn make_card(
        table_name: &str,
        row_count: i64,
        cols: Vec<(&str, &str, bool, bool)>, // (name, type, is_pk, is_fk)
        fks: Vec<(&str, &str, &str)>,        // (col, ref_table, ref_col)
    ) -> TableCard {
        let columns = cols
            .into_iter()
            .map(|(name, type_, is_pk, is_fk)| crate::nl2sql::spec::CardColumn {
                name: name.to_string(),
                type_: type_.to_string(),
                nullable: !is_pk,
                is_primary_key: is_pk,
                is_foreign_key: is_fk,
                sample_values: vec![],
                profile: crate::nl2sql::spec::ColumnProfile {
                    approximate_distinct_count: None,
                    null_ratio: None,
                    min: None,
                    max: None,
                    sampled_rows: 0,
                },
            })
            .collect();
        let fk_edges = fks
            .into_iter()
            .map(|(col, ref_table, ref_col)| CardFkEdge {
                column: col.to_string(),
                ref_table: ref_table.to_string(),
                ref_column: ref_col.to_string(),
            })
            .collect();
        TableCard {
            source_id: "test".to_string(),
            table_name: table_name.to_string(),
            row_count,
            columns,
            fk_edges,
            card_vector: None,
            card_text: table_name.to_string(),
        }
    }

    /// Verifies the fix to PersonFullName's bare "name" token.
    ///
    /// (a) Genuine person-name columns from the real schemas must still
    ///     classify as a PII person-name role with is_pii == true.
    /// (b) Non-person "*_name" columns from the real schemas (dev and alt)
    ///     must NOT be PersonFullName, must NOT be is_pii, and must reach
    ///     ColumnRole::Description — previously they were all PersonFullName
    ///     because the bare "name" substring token matched every column whose
    ///     name contained the substring "name".
    #[test]
    fn name_token_pii_classification() {
        // (a) Genuine person-name columns
        let person_card = make_card(
            "people_test",
            100,
            vec![
                // PersonFullName: only the qualified tokens remain
                ("full_name",   "varchar", false, false),
                // PersonGivenName: first_name, middle_name
                ("first_name",  "varchar", false, false),
                ("middle_name", "varchar", false, false),
                // PersonFamilyName: last_name
                ("last_name",   "varchar", false, false),
            ],
            vec![],
        );
        // (b) Non-person "*_name" columns from dev schema:
        //   generic_name (medication_catalog), brand_name (medication_catalog),
        //   panel_name (lab_test_catalog / lab_orders), test_name (lab_results),
        //   condition_name (patient_medical_history / patient_family_history),
        //   procedure_name (procedures), short_name (insurance_providers),
        //   file_name (patient_documents), disease_name (mortality_records),
        //   clinic_name (provider_schedules).
        // Also bare "name" columns from: counties, departments, wards,
        //   allergen_catalog, vaccine_catalog, insurance_providers,
        //   care_programs, equipment.
        // Alt schema: generic_name, brand_name (DrugMaster), name (TestCatalog /
        //   ClinDept).
        let non_person_card = make_card(
            "non_person_test",
            100,
            vec![
                ("generic_name",   "varchar", false, false),
                ("brand_name",     "varchar", false, false),
                ("panel_name",     "varchar", false, false),
                ("test_name",      "varchar", false, false),
                ("condition_name", "varchar", false, false),
                ("procedure_name", "varchar", false, false),
                ("short_name",     "varchar", false, false),
                ("file_name",      "varchar", false, false),
                ("disease_name",   "varchar", false, false),
                ("clinic_name",    "varchar", false, false),
            ],
            vec![],
        );
        // Bare "name" columns (departments, wards, insurance_providers, etc.)
        let bare_name_card = make_card(
            "departments_test",
            100,
            vec![("name", "varchar", false, false)],
            vec![],
        );

        let bindings = bind_cards(
            &[person_card, non_person_card, bare_name_card],
            0.05, 3, None, None, &HashMap::new(),
        );

        let person_tb = bindings.iter().find(|t| t.table_name == "people_test").unwrap();

        // (a) Person-name columns — all must be PII
        {
            let cb = person_tb.columns.iter().find(|c| c.column_name == "full_name").unwrap();
            assert_eq!(cb.role, ColumnRole::PersonFullName, "full_name must be PersonFullName");
            assert!(cb.is_pii, "full_name must be is_pii");
        }
        {
            let cb = person_tb.columns.iter().find(|c| c.column_name == "first_name").unwrap();
            assert_eq!(cb.role, ColumnRole::PersonGivenName, "first_name must be PersonGivenName");
            assert!(cb.is_pii, "first_name must be is_pii");
        }
        {
            let cb = person_tb.columns.iter().find(|c| c.column_name == "middle_name").unwrap();
            assert_eq!(cb.role, ColumnRole::PersonGivenName, "middle_name must be PersonGivenName");
            assert!(cb.is_pii, "middle_name must be is_pii");
        }
        {
            let cb = person_tb.columns.iter().find(|c| c.column_name == "last_name").unwrap();
            assert_eq!(cb.role, ColumnRole::PersonFamilyName, "last_name must be PersonFamilyName");
            assert!(cb.is_pii, "last_name must be is_pii");
        }

        // (b) Non-person "*_name" columns — NOT PersonFullName, role.is_pii()==false,
        //     and reach Description.
        //
        // Note: ColumnBinding.is_pii is the OR of the role-level is_pii() AND the
        // connectors::routes::is_likely_pii() keyword heuristic (which also carries a
        // bare "name" substring keyword for conservative intake PII tagging).  The
        // relevant invariant for this fix is the ROLE-level classification: none of
        // these columns should be assigned PersonFullName (whose is_pii() == true),
        // and each should reach Description (whose is_pii() == false).  The binding-
        // level is_pii field is not checked here because it is controlled by a
        // separate, intentionally-conservative system outside src/ontology.
        let non_person_tb = bindings.iter().find(|t| t.table_name == "non_person_test").unwrap();
        for col_name in [
            "generic_name", "brand_name", "panel_name", "test_name",
            "condition_name", "procedure_name", "short_name", "file_name",
            "disease_name", "clinic_name",
        ] {
            let cb = non_person_tb.columns.iter().find(|c| c.column_name == col_name).unwrap();
            assert_ne!(cb.role, ColumnRole::PersonFullName,
                "{col_name} must NOT be PersonFullName");
            assert!(!cb.role.is_pii(),
                "{col_name} role.is_pii() must be false (role={:?})", cb.role);
            assert_eq!(cb.role, ColumnRole::Description,
                "{col_name} must reach Description");
        }

        // Bare "name" column (departments, wards, allergen_catalog, etc.)
        let bare_tb = bindings.iter().find(|t| t.table_name == "departments_test").unwrap();
        let bare_col = bare_tb.columns.iter().find(|c| c.column_name == "name").unwrap();
        assert_ne!(bare_col.role, ColumnRole::PersonFullName,
            "bare 'name' column must NOT be PersonFullName");
        assert!(!bare_col.role.is_pii(),
            "bare 'name' role.is_pii() must be false");
        assert_eq!(bare_col.role, ColumnRole::Description,
            "bare 'name' column must reach Description");
    }

    #[test]
    fn patient_path_zero_hops() {
        // The patient table itself should have path = Some([])
        let cards = vec![
            make_card("patients", 1000, vec![("id", "serial", true, false), ("dob", "date", false, false)], vec![]),
            make_card("encounters", 5000, vec![
                ("id", "serial", true, false),
                ("patient_id", "integer", false, true),
                ("visit_date", "date", false, false),
            ], vec![("patient_id", "patients", "id")]),
        ];

        let bindings = bind_cards(&cards, 0.25, 3, None, None, &HashMap::new());
        let patient = bindings.iter().find(|t| t.table_name == "patients").unwrap();
        assert_eq!(
            patient.patient_path,
            Some(vec![]),
            "patient table should have empty patient_path (zero hops)"
        );
    }

    #[test]
    fn vital_signs_patient_path_le_2_hops() {
        // vitals → patient ≤ 2 hops (acceptance criterion 4 flavour)
        let cards = vec![
            make_card("patients", 1000, vec![
                ("id", "serial", true, false),
                ("date_of_birth", "date", false, false),
            ], vec![]),
            make_card("encounters", 5000, vec![
                ("id", "serial", true, false),
                ("patient_id", "integer", false, true),
                ("visit_date", "date", false, false),
            ], vec![("patient_id", "patients", "id")]),
            make_card("vital_signs", 20000, vec![
                ("id", "serial", true, false),
                ("encounter_id", "integer", false, true),
                ("temperature", "numeric", false, false),
                ("pulse", "integer", false, false),
                ("recorded_at", "timestamp", false, false),
            ], vec![("encounter_id", "encounters", "id")]),
        ];

        let bindings = bind_cards(&cards, 0.25, 3, None, None, &HashMap::new());
        let vitals = bindings.iter().find(|t| t.table_name == "vital_signs").unwrap();
        let path = vitals.patient_path.as_ref().expect("vital_signs should be reachable");
        assert!(
            path.len() <= 2,
            "vital_signs→patients should be ≤2 hops, got {}",
            path.len()
        );
    }

    #[test]
    fn event_time_prefers_domain_over_created_at() {
        // `created_at` should not be chosen when a domain date exists (criterion 5)
        let cards = vec![
            make_card("lab_results", 10000, vec![
                ("id", "serial", true, false),
                ("order_id", "integer", false, true),
                ("result_date", "date", false, false),
                ("created_at", "timestamp", false, false),
            ], vec![]),
        ];
        let bindings = bind_cards(&cards, 0.10, 3, None, None, &HashMap::new());
        let tb = &bindings[0];
        assert_ne!(
            tb.event_time_col.as_deref(),
            Some("created_at"),
            "created_at should not be chosen when a domain date column exists"
        );
        assert_eq!(tb.event_time_col.as_deref(), Some("result_date"));
    }

    #[test]
    fn pii_col_empty_enum_values() {
        // No ColumnBinding with is_pii==true should have non-empty enum_values (criterion 6)
        let cards = vec![make_card("patients", 1000, vec![
            ("id", "serial", true, false),
            ("first_name", "varchar", false, false),
            ("last_name", "varchar", false, false),
            ("phone", "varchar", false, false),
            ("gender", "varchar", false, false),
        ], vec![])];
        let bindings = bind_cards(&cards, 0.10, 3, None, None, &HashMap::new());
        for tb in &bindings {
            for cb in &tb.columns {
                if cb.is_pii {
                    assert!(
                        cb.enum_values.is_empty(),
                        "PII column {} must have empty enum_values",
                        cb.column_name
                    );
                }
            }
        }
    }

    #[test]
    fn override_forces_concept() {
        // Acceptance criterion 9
        let cards = vec![
            make_card("patients", 1000, vec![("id", "serial", true, false)], vec![]),
            make_card("encounters", 5000, vec![
                ("id", "serial", true, false),
                ("patient_id", "integer", false, true),
            ], vec![("patient_id", "patients", "id")]),
        ];
        let mut ov = BindingOverrides::default();
        ov.table_concepts.insert("patients".into(), Some(EntityConcept::Provider));

        let bindings = bind_cards(&cards, 0.20, 3, None, Some(&ov), &HashMap::new());
        let pat = bindings.iter().find(|t| t.table_name == "patients").unwrap();
        assert_eq!(pat.concept, EntityConcept::Provider, "override should force concept");
    }

    #[test]
    fn override_concept_none_removes() {
        // concept: None removes table from service lines
        let cards = vec![
            make_card("patients", 1000, vec![("id", "serial", true, false)], vec![]),
        ];
        let mut ov = BindingOverrides::default();
        ov.table_concepts.insert("patients".into(), None);

        let bindings = bind_cards(&cards, 0.20, 3, None, Some(&ov), &HashMap::new());
        let pat = bindings.iter().find(|t| t.table_name == "patients").unwrap();
        assert_eq!(pat.concept, EntityConcept::Unknown, "concept: None should result in Unknown");
        assert!(pat.service_lines.is_empty(), "Unknown concept should have no service lines");
    }

    #[test]
    fn determinism_shuffled_order() {
        // Acceptance criterion 10: shuffled card order → identical SchemaBinding
        let mut cards = vec![
            make_card("patients", 1000, vec![
                ("id", "serial", true, false),
                ("date_of_birth", "date", false, false),
            ], vec![]),
            make_card("encounters", 5000, vec![
                ("id", "serial", true, false),
                ("patient_id", "integer", false, true),
                ("visit_date", "date", false, false),
            ], vec![("patient_id", "patients", "id")]),
            make_card("vital_signs", 20000, vec![
                ("id", "serial", true, false),
                ("encounter_id", "integer", false, true),
                ("recorded_at", "timestamp", false, false),
            ], vec![("encounter_id", "encounters", "id")]),
        ];
        let b1 = bind_cards(&cards, 0.20, 3, None, None, &HashMap::new());

        // Shuffle
        cards.reverse();
        let b2 = bind_cards(&cards, 0.20, 3, None, None, &HashMap::new());

        // Results should be identical (both sorted by table_name)
        assert_eq!(b1.len(), b2.len());
        for (a, b) in b1.iter().zip(b2.iter()) {
            assert_eq!(a.table_name, b.table_name, "table order mismatch after shuffle");
            assert_eq!(a.concept, b.concept, "concept changed after shuffle for {}", a.table_name);
        }
    }
}
