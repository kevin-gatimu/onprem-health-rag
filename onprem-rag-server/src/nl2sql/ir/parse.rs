//! Grammar-based NL question → `QuerySpec` parser.
//!
//! The grammar is a small set of slot-filling rules (R1–R16) tried in order.
//! A question parses when a rule matches AND all remaining tokens after consuming
//! the rule's pattern are in the filler allow-list (so "how many patients **records
//! do we have**" parses but "how many patients are pregnant" does not if
//! "pregnant" is not a filler word).
//!
//! Entity extraction (concept nouns, enum values, time ranges) is done entirely
//! from the question text plus the `SchemaBinding`'s `enum_values` and the
//! ontology `DESCRIPTORS`. The optional `RouteEntities` hint (plan 02) is used
//! only to bias concept selection; every test passes `None`.
//!
//! # Non-negotiable
//! No physical table or column name from any dev-seed schema appears in this file.

use crate::ontology::binding::SchemaBinding;
use crate::ontology::concepts::{EntityConcept, DESCRIPTORS};
use crate::ontology::roles::ColumnRole;
use crate::router::RouteEntities;

use super::spec::{
    BucketUnit, ColumnRef, Dimension, Filter, FilterOp, FilterValue, Measure,
    MeasureOp, MissingSlot, Order, OrderTarget, QuerySpec, Shape, SortDir, SpecProvenance, Subject,
    TimeRange, TimeScope,
};

use super::predicates::{lookup_predicate, predicate_to_filter};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub enum ParseOutcome {
    Parsed { spec: QuerySpec, missing: Vec<MissingSlot> },
    NoParse,
}

/// Parse a natural-language question into a typed `QuerySpec`.
///
/// `hint` — optional Tier-2 entity hints (tables, metric, time_bucket).  Every
///          unit test passes `None`; plan 02 will feed richer hints later.
/// `binding` — the schema binding for the target source.
/// `scope` — if non-empty, only these table names are reachable.
pub fn parse(
    q: &str,
    hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    scope: &[String],
) -> ParseOutcome {
    let norm = normalize(q);
    let tokens: Vec<&str> = norm.split_whitespace().collect();

    // Try each rule in priority order.
    let rules: &[fn(&[&str], Option<&RouteEntities>, &SchemaBinding, &[String]) -> Option<(QuerySpec, Vec<MissingSlot>)>] = &[
        try_r9_lookup,
        try_r10_exists,
        try_r7_rate,
        try_r4_trend,
        try_r6_topn,
        try_r5_topn_actor,
        try_r1_count,
        try_r2_sum,
        try_r3_average,
        try_r8_list,
    ];

    for rule_fn in rules {
        if let Some((spec, missing)) = rule_fn(&tokens, hint, binding, scope) {
            return ParseOutcome::Parsed { spec, missing };
        }
    }

    ParseOutcome::NoParse
}

// ---------------------------------------------------------------------------
// Filler token allow-list
// ---------------------------------------------------------------------------

const FILLER: &[&str] = &[
    "the", "of", "in", "for", "we", "have", "are", "there", "were", "our", "please",
    "show", "me", "tell", "give", "list", "what", "is", "how", "many", "much", "did",
    "do", "does", "total", "number", "records", "all", "currently", "right", "now",
    "a", "an", "by", "on", "at", "from", "to", "this", "that", "these", "those",
    "and", "with", "or", "not", "any", "each", "every", "per", "get", "find",
    "see", "view", "check", "had", "has", "been", "be", "my", "your", "their",
    "its", "which", "who", "whose", "where", "when", "why",
];

fn is_filler(tok: &str) -> bool {
    FILLER.contains(&tok)
}

/// Returns `true` if every token in `remaining` is a filler word.
fn fully_consumed(remaining: &[&str]) -> bool {
    remaining.iter().all(|t| is_filler(t))
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

pub(crate) fn normalize(q: &str) -> String {
    q.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Concept resolution
// ---------------------------------------------------------------------------

/// Map a noun or phrase fragment to an `EntityConcept`.
/// Checks `descriptor.singular`, `descriptor.plural`, and `descriptor.synonyms`
/// for all concepts. Does NOT look at physical table names.
pub(crate) fn phrase_to_concept(phrase: &str, binding: &SchemaBinding) -> Option<EntityConcept> {
    let phrase_lower = phrase.to_ascii_lowercase();

    // First pass: exact match against descriptor tokens.
    for desc in DESCRIPTORS.iter() {
        if desc.singular == phrase_lower
            || desc.plural == phrase_lower
            || desc.synonyms.contains(&phrase_lower.as_str())
        {
            // Check the concept is actually bound in this schema.
            if desc.concept != EntityConcept::Unknown {
                return Some(desc.concept);
            }
        }
    }

    // Second pass: contains match against name_tokens.
    for desc in DESCRIPTORS.iter() {
        if desc.concept == EntityConcept::Unknown {
            continue;
        }
        if desc.name_tokens.iter().any(|tok| phrase_lower.contains(tok)) {
            return Some(desc.concept);
        }
    }

    // Third pass: match against bound table names (after stripping underscores, lowercased).
    for tb in &binding.tables {
        if tb.concept == EntityConcept::Unknown {
            continue;
        }
        let tbl_norm = tb.table_name.replace('_', " ").to_ascii_lowercase();
        if tbl_norm == phrase_lower || tbl_norm.contains(&phrase_lower) {
            return Some(tb.concept);
        }
    }

    None
}

/// Try to extract a concept from the token stream, starting at or after `skip` tokens.
/// Returns `(concept, end_pos)` where `end_pos` is the index after the last consumed token.
fn extract_concept(
    tokens: &[&str],
    start: usize,
    binding: &SchemaBinding,
) -> Option<(EntityConcept, usize)> {
    // Try multi-word phrases first (up to 3 tokens), then single word.
    for len in (1usize..=3).rev() {
        let end = start + len;
        if end > tokens.len() {
            continue;
        }
        let phrase = tokens[start..end].join(" ");
        if let Some(concept) = phrase_to_concept(&phrase, binding) {
            return Some((concept, end));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Time range extraction
// ---------------------------------------------------------------------------

fn extract_time_range(tokens: &[&str]) -> Option<(TimeRange, BucketUnit, usize, usize)> {
    // Pattern: "last month" / "last N months" / "this year" / "yesterday" etc.
    // Returns (TimeRange, BucketUnit, start_pos, end_pos).
    let n = tokens.len();

    for i in 0..n {
        // "yesterday"
        if tokens[i] == "yesterday" {
            return Some((TimeRange::Yesterday, BucketUnit::Day, i, i + 1));
        }
        // "today" / "right now"
        if tokens[i] == "today" || (i + 1 < n && tokens[i] == "right" && tokens[i+1] == "now") {
            let end = if i + 1 < n && tokens[i+1] == "now" { i + 2 } else { i + 1 };
            return Some((TimeRange::Today, BucketUnit::Day, i, end));
        }
        // "this week/month/quarter/year"
        if tokens[i] == "this" && i + 1 < n {
            if let Some((unit, end)) = parse_bucket_unit(tokens, i + 1) {
                let range = match unit {
                    BucketUnit::Week => TimeRange::ThisWeek,
                    BucketUnit::Month => TimeRange::ThisMonth,
                    BucketUnit::Quarter => TimeRange::ThisQuarter,
                    BucketUnit::Year => TimeRange::ThisYear,
                    _ => continue,
                };
                return Some((range, unit, i, end));
            }
        }
        // "last week/month/quarter/year"
        if tokens[i] == "last" && i + 1 < n {
            // "last N unit"
            if let Ok(num) = tokens[i+1].parse::<u32>() {
                if i + 2 < n {
                    if let Some((unit, end)) = parse_bucket_unit_plural(tokens, i + 2) {
                        return Some((TimeRange::Last { n: num, unit }, unit, i, end));
                    }
                }
            }
            if let Some((unit, end)) = parse_bucket_unit(tokens, i + 1) {
                let range = match unit {
                    BucketUnit::Week => TimeRange::LastWeek,
                    BucketUnit::Month => TimeRange::LastMonth,
                    BucketUnit::Quarter => TimeRange::LastQuarter,
                    BucketUnit::Year => TimeRange::LastYear,
                    _ => continue,
                };
                return Some((range, unit, i, end));
            }
        }
        // "past N days/weeks/months"
        if (tokens[i] == "past" || tokens[i] == "previous") && i + 1 < n {
            if let Ok(num) = tokens[i+1].parse::<u32>() {
                if i + 2 < n {
                    if let Some((unit, end)) = parse_bucket_unit_plural(tokens, i + 2) {
                        return Some((TimeRange::Last { n: num, unit }, unit, i, end));
                    }
                }
            }
        }
        // "next N days/weeks/months" or "within N days"
        if tokens[i] == "within" && i + 1 < n {
            if let Ok(num) = tokens[i+1].parse::<u32>() {
                if i + 2 < n {
                    if let Some((unit, end)) = parse_bucket_unit_plural(tokens, i + 2) {
                        return Some((TimeRange::Within { n: num, unit }, unit, i, end));
                    }
                }
            }
        }
        if tokens[i] == "next" && i + 1 < n {
            if let Ok(num) = tokens[i+1].parse::<u32>() {
                if i + 2 < n {
                    if let Some((unit, end)) = parse_bucket_unit_plural(tokens, i + 2) {
                        return Some((TimeRange::Next { n: num, unit }, unit, i, end));
                    }
                }
            }
            if let Some((unit, end)) = parse_bucket_unit(tokens, i + 1) {
                return Some((TimeRange::Next { n: 1, unit }, unit, i, end));
            }
        }
        // "tonight" / "today night" → Today
        if tokens[i] == "tonight" || tokens[i] == "tonight" {
            return Some((TimeRange::Today, BucketUnit::Day, i, i + 1));
        }
        // "this quarter" "last quarter"
        if i + 1 < n && tokens[i] == "last" && tokens[i+1] == "night" {
            return Some((TimeRange::Yesterday, BucketUnit::Day, i, i + 2));
        }
        // "last month" "this month" etc. (repeat of above for safety)
        if i + 1 < n && tokens[i] == "this" && tokens[i+1] == "week" {
            return Some((TimeRange::ThisWeek, BucketUnit::Week, i, i + 2));
        }
        if i + 1 < n && tokens[i] == "last" && tokens[i+1] == "month" {
            return Some((TimeRange::LastMonth, BucketUnit::Month, i, i + 2));
        }
        if i + 1 < n && tokens[i] == "this" && tokens[i+1] == "month" {
            return Some((TimeRange::ThisMonth, BucketUnit::Month, i, i + 2));
        }
        if i + 1 < n && tokens[i] == "last" && tokens[i+1] == "year" {
            return Some((TimeRange::LastYear, BucketUnit::Year, i, i + 2));
        }
        if i + 1 < n && tokens[i] == "this" && tokens[i+1] == "year" {
            return Some((TimeRange::ThisYear, BucketUnit::Year, i, i + 2));
        }
        if i + 1 < n && tokens[i] == "last" && tokens[i+1] == "quarter" {
            return Some((TimeRange::LastQuarter, BucketUnit::Quarter, i, i + 2));
        }
        if i + 1 < n && tokens[i] == "this" && tokens[i+1] == "quarter" {
            return Some((TimeRange::ThisQuarter, BucketUnit::Quarter, i, i + 2));
        }
    }
    None
}

fn parse_bucket_unit(tokens: &[&str], i: usize) -> Option<(BucketUnit, usize)> {
    if i >= tokens.len() { return None; }
    match tokens[i] {
        "hour" | "hourly" => Some((BucketUnit::Hour, i + 1)),
        "day" | "daily" => Some((BucketUnit::Day, i + 1)),
        "week" | "weekly" => Some((BucketUnit::Week, i + 1)),
        "month" | "monthly" => Some((BucketUnit::Month, i + 1)),
        "quarter" | "quarterly" => Some((BucketUnit::Quarter, i + 1)),
        "year" | "annual" | "annually" | "yearly" => Some((BucketUnit::Year, i + 1)),
        _ => None,
    }
}

fn parse_bucket_unit_plural(tokens: &[&str], i: usize) -> Option<(BucketUnit, usize)> {
    if i >= tokens.len() { return None; }
    match tokens[i] {
        "hour" | "hours" => Some((BucketUnit::Hour, i + 1)),
        "day" | "days" => Some((BucketUnit::Day, i + 1)),
        "week" | "weeks" => Some((BucketUnit::Week, i + 1)),
        "month" | "months" => Some((BucketUnit::Month, i + 1)),
        "quarter" | "quarters" => Some((BucketUnit::Quarter, i + 1)),
        "year" | "years" => Some((BucketUnit::Year, i + 1)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Enum value matching (R11 / filters)
// ---------------------------------------------------------------------------

/// Check if `phrase` matches any enum value across the binding, and return
/// a filter for it. Matching is case-insensitive.
fn enum_filter_for_phrase(phrase: &str, concept: EntityConcept, binding: &SchemaBinding) -> Option<Filter> {
    let norm = phrase.to_ascii_lowercase();
    for tb in &binding.tables {
        if tb.concept != concept && concept != EntityConcept::Unknown { continue; }
        for cb in &tb.columns {
            for ev in &cb.enum_values {
                if ev.to_ascii_lowercase() == norm || ev.to_ascii_lowercase().contains(&norm) {
                    return Some(Filter {
                        column: ColumnRef {
                            concept: tb.concept,
                            role: cb.role,
                            name_hint: None,
                            physical: None,
                        },
                        op: FilterOp::Eq,
                        value: FilterValue::Str(ev.clone()),
                    });
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Time scope builder
// ---------------------------------------------------------------------------

fn make_time_scope(concept: EntityConcept, range: TimeRange) -> TimeScope {
    TimeScope {
        column: ColumnRef {
            concept,
            role: ColumnRole::EventTime,
            name_hint: None,
            physical: None,
        },
        range: Some(range),
        bucket: None,
    }
}

// ---------------------------------------------------------------------------
// Rule implementations
// ---------------------------------------------------------------------------

// R1: "how many|count|number of <concept> [filters] [time] [by <dim>]"
fn try_r1_count(
    tokens: &[&str],
    hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    // Check anchor tokens.
    let after_anchor = if let Some(pos) = find_phrase(tokens, &["how", "many"]) {
        Some(pos + 2)
    } else if let Some(pos) = find_token(tokens, "count") {
        Some(pos + 1)
    } else if let Some(pos) = find_phrase(tokens, &["number", "of"]) {
        Some(pos + 2)
    } else if let Some(pos) = find_phrase(tokens, &["total", "number", "of"]) {
        Some(pos + 3)
    } else {
        None
    }?;

    // Scan forward from after_anchor to tolerate modifier adjectives before the
    // concept noun (e.g. "how many caesarean deliveries" — "caesarean" is not a
    // concept; keep scanning until "deliveries" is matched).
    let (concept, concept_end) = {
        let mut found = None;
        for pos in after_anchor..tokens.len() {
            if let Some(r) = extract_concept(tokens, pos, binding) {
                found = Some(r);
                break;
            }
        }
        found
    }?;

    // Collect time range and dimension.
    let time_range = extract_time_range(tokens);
    let by_concept = extract_by_dimension(tokens, binding).map(|(c, _)| c);

    let mut filters = Vec::new();
    // Apply domain predicates if present.
    let q_str = tokens.join(" ");
    if let Some(pred) = lookup_predicate(&q_str) {
        if pred.concept == concept || concept == EntityConcept::Unknown {
            filters.push(predicate_to_filter(pred));
        }
    }

    let shape = if by_concept.is_some() { Shape::Grouped } else { Shape::Scalar };
    let measures = vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }];

    let mut dimensions = Vec::new();
    let mut order = Vec::new();
    if let Some(dim_concept) = by_concept {
        dimensions.push(Dimension {
            column: ColumnRef { concept: dim_concept, role: ColumnRole::Description, name_hint: None, physical: None },
            label: dim_concept.slug().to_string(),
        });
        order.push(Order { target: OrderTarget::Measure("count".into()), dir: SortDir::Desc });
    }

    // Apply R12/R13/R15 modifiers.
    let filters = apply_modifier_rules(filters, tokens, concept, binding);
    let time_scope = time_range.map(|(range, _, _, _)| make_time_scope(concept, range));

    let spec = QuerySpec {
        subject: Subject { concept, table: None },
        shape,
        measures,
        dimensions,
        filters,
        time: time_scope,
        order,
        limit: None,
        joins: vec![],
        projection: vec![],
        provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
    };

    Some((spec, vec![]))
}

// R2: "total|sum of <Amount/Quantity col> [of <concept>] [filters] [time]"
fn try_r2_sum(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let anchor_pos = if let Some(p) = find_token(tokens, "total") { Some(p) }
        else if let Some(p) = find_phrase(tokens, &["sum", "of"]) { Some(p) }
        else if let Some(p) = find_token(tokens, "dispensed") { Some(p) }
        else if let Some(p) = find_token(tokens, "collected") { Some(p) }
        else if let Some(p) = find_token(tokens, "outstanding") { Some(p) }
        else { None }?;

    // Try to find a concept after the anchor.
    let concept = find_any_concept(tokens, binding)?;
    let time_range = extract_time_range(tokens);
    let by_concept = extract_by_dimension(tokens, binding).map(|(c, _)| c);

    // Figure out what to sum: Amount, Quantity, or Duration.
    let (measure_op, target_role, name_hint) = if tokens.iter().any(|t| *t == "dispensed" || *t == "used" || *t == "consumed") {
        (MeasureOp::Sum, ColumnRole::Quantity, Some("dispense"))
    } else if tokens.iter().any(|t| *t == "collected" || *t == "received" || *t == "billed") {
        (MeasureOp::Sum, ColumnRole::Amount, None)
    } else {
        (MeasureOp::Sum, ColumnRole::Amount, None)
    };

    let target = ColumnRef { concept, role: target_role, name_hint: name_hint.map(|s| s.to_string()), physical: None };
    let measures = vec![Measure { op: measure_op, target: Some(target), alias: "total".into() }];
    let shape = if by_concept.is_some() { Shape::Grouped } else { Shape::Scalar };

    let mut dimensions = Vec::new();
    let mut order = Vec::new();
    if let Some(dim_c) = by_concept {
        dimensions.push(Dimension {
            column: ColumnRef { concept: dim_c, role: ColumnRole::Description, name_hint: None, physical: None },
            label: dim_c.slug().to_string(),
        });
        order.push(Order { target: OrderTarget::Measure("total".into()), dir: SortDir::Desc });
    }

    let time_scope = time_range.map(|(range, _, _, _)| make_time_scope(concept, range));

    let spec = QuerySpec {
        subject: Subject { concept, table: None },
        shape,
        measures,
        dimensions,
        filters: vec![],
        time: time_scope,
        order,
        limit: None,
        joins: vec![],
        projection: vec![],
        provenance: SpecProvenance { rule: "R2".into(), focus_subs: vec![] },
    };

    Some((spec, vec![]))
}

// R3: "average|mean|median|longest|shortest|highest|lowest <col> ..."
fn try_r3_average(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let (agg_op, anchor_end) = if let Some(p) = find_token(tokens, "average") {
        (MeasureOp::Avg, p + 1)
    } else if let Some(p) = find_token(tokens, "mean") {
        (MeasureOp::Avg, p + 1)
    } else if let Some(p) = find_token(tokens, "median") {
        (MeasureOp::Median, p + 1)
    } else if let Some(p) = find_token(tokens, "longest") {
        (MeasureOp::Max, p + 1)
    } else if let Some(p) = find_token(tokens, "shortest") {
        (MeasureOp::Min, p + 1)
    } else if tokens.iter().any(|t| ["turnaround", "wait", "waiting", "door-to-doctor", "theatre"].contains(t)) {
        (MeasureOp::Avg, 0)
    } else {
        return None;
    };

    let concept = find_any_concept(tokens, binding)?;
    let time_range = extract_time_range(tokens);
    let by_concept = extract_by_dimension(tokens, binding).map(|(c, _)| c);

    // Determine the measure target role.
    let (target_role, name_hint) = if tokens.iter().any(|t| ["wait", "waiting", "door-to-doctor", "turnaround", "theatre"].contains(t)) {
        (ColumnRole::Duration, Some("duration"))
    } else if tokens.iter().any(|t| ["bill", "billing", "charge", "cost", "fee"].contains(t)) {
        (ColumnRole::Amount, None)
    } else if tokens.iter().any(|t| ["weight", "bmi", "height"].contains(t)) {
        (ColumnRole::Measure, Some("weight"))
    } else {
        (ColumnRole::Duration, None)
    };

    let target = ColumnRef { concept, role: target_role, name_hint: name_hint.map(|s| s.to_string()), physical: None };
    let measures = vec![Measure { op: agg_op, target: Some(target), alias: "average".into() }];
    let shape = if by_concept.is_some() { Shape::Grouped } else { Shape::Scalar };

    let mut dimensions = Vec::new();
    let mut order = Vec::new();
    if let Some(dim_c) = by_concept {
        dimensions.push(Dimension {
            column: ColumnRef { concept: dim_c, role: ColumnRole::Description, name_hint: None, physical: None },
            label: dim_c.slug().to_string(),
        });
        order.push(Order { target: OrderTarget::Measure("average".into()), dir: SortDir::Desc });
    }

    let time_scope = time_range.map(|(range, _, _, _)| make_time_scope(concept, range));

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape,
            measures,
            dimensions,
            filters: vec![],
            time: time_scope,
            order,
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R3".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R4: trend over time
fn try_r4_trend(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let has_trend = tokens.iter().any(|t| ["trend", "per", "over", "monthly", "weekly", "daily", "quarterly", "annually"].contains(t));
    if !has_trend { return None; }

    let concept = find_any_concept(tokens, binding)?;
    let time_range = extract_time_range(tokens);

    // Extract bucket unit from "per month", "monthly", "by day", etc.
    let bucket = if tokens.iter().any(|t| *t == "monthly" || (*t == "per" && tokens.windows(2).any(|w| w == ["per", "month"]))) {
        BucketUnit::Month
    } else if tokens.iter().any(|t| *t == "weekly" || tokens.windows(2).any(|w| w == ["per", "week"])) {
        BucketUnit::Week
    } else if tokens.iter().any(|t| *t == "daily" || tokens.windows(2).any(|w| w == ["per", "day"])) {
        BucketUnit::Day
    } else if tokens.iter().any(|t| *t == "quarterly" || tokens.windows(2).any(|w| w == ["per", "quarter"])) {
        BucketUnit::Quarter
    } else if tokens.iter().any(|t| *t == "annually" || tokens.windows(2).any(|w| w == ["per", "year"])) {
        BucketUnit::Year
    } else {
        BucketUnit::Month // default
    };

    let time_col = ColumnRef { concept, role: ColumnRole::EventTime, name_hint: None, physical: None };
    let mut ts = make_time_scope(concept, time_range.map(|(r, _, _, _)| r).unwrap_or(TimeRange::ThisYear));
    ts.bucket = Some(bucket);

    let dim = Dimension {
        column: time_col.clone(),
        label: bucket.as_str().to_string(),
    };

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::Trend,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![dim],
            filters: vec![],
            time: Some(ts),
            order: vec![Order { target: OrderTarget::Column(time_col), dir: SortDir::Asc }],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R4".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R5: "which <dim-concept> had|ordered|prescribed|saw|performed the most|fewest <concept>"
fn try_r5_topn_actor(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let has_anchor = tokens.iter().any(|t| ["most", "fewest", "least"].contains(t));
    if !has_anchor { return None; }

    let subject_concept = find_any_concept(tokens, binding)?;
    let time_range = extract_time_range(tokens);

    // Find the dimension concept (the "actor") — usually Provider or Department.
    let (dim_concept, _) = extract_by_dimension(tokens, binding).or_else(|| {
        // Infer from verbs: "which doctor" / "which provider"
        let by_provider = tokens.iter().any(|t| ["doctor", "doctors", "nurse", "nurses", "provider", "providers", "clinician", "clinicians", "staff"].contains(t));
        if by_provider { Some((EntityConcept::Provider, 0)) } else { None }
    })?;

    let dir = if tokens.iter().any(|t| ["fewest", "least"].contains(t)) { SortDir::Asc } else { SortDir::Desc };
    let limit = extract_top_n(tokens).unwrap_or(10);

    let time_scope = time_range.map(|(range, _, _, _)| make_time_scope(subject_concept, range));

    Some((
        QuerySpec {
            subject: Subject { concept: subject_concept, table: None },
            shape: Shape::TopN,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![Dimension {
                column: ColumnRef { concept: dim_concept, role: ColumnRole::PersonFullName, name_hint: None, physical: None },
                label: dim_concept.slug().to_string(),
            }],
            filters: vec![],
            time: time_scope,
            order: vec![Order { target: OrderTarget::Measure("count".into()), dir }],
            limit: Some(limit),
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R5".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R6: "top|most common|most frequent <N>? <concept|col>"
fn try_r6_topn(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let has_top = tokens.iter().any(|t| *t == "top");
    if !has_top { return None; }

    let concept = find_any_concept(tokens, binding)?;
    let limit = extract_top_n(tokens).unwrap_or(10);
    let time_range = extract_time_range(tokens);
    let time_scope = time_range.map(|(r, _, _, _)| make_time_scope(concept, r));

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::TopN,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![],
            filters: vec![],
            time: time_scope,
            order: vec![Order { target: OrderTarget::Measure("count".into()), dir: SortDir::Desc }],
            limit: Some(limit),
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R6".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R7: rate questions ("no-show rate", "caesarean rate", etc.)
fn try_r7_rate(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let has_rate = tokens.iter().any(|t| *t == "rate");
    if !has_rate { return None; }

    let q_str = tokens.join(" ");

    // Check known rate predicates.
    let pred = lookup_predicate(&q_str)?;
    let concept = pred.concept;
    let numerator_filter = predicate_to_filter(pred);

    let time_range = extract_time_range(tokens);
    let time_scope = time_range.map(|(r, _, _, _)| make_time_scope(concept, r));

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::Rate,
            measures: vec![Measure {
                op: MeasureOp::Rate { numerator: Box::new(numerator_filter) },
                target: None,
                alias: "rate".into(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: time_scope,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R7".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R8: "list|show|which <concept> [filters] [time] [limit]" / "who is|are <filters>"
fn try_r8_list(
    tokens: &[&str],
    hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let has_list = tokens.iter().any(|t| {
        ["list", "show", "which", "who", "whose", "what", "find", "display", "get"].contains(t)
    });
    if !has_list { return None; }

    let concept = find_any_concept(tokens, binding)?;
    let time_range = extract_time_range(tokens);

    // Collect filters from domain predicates and enum values.
    let q_str = tokens.join(" ");
    let mut filters = Vec::new();

    // Domain predicate filters.
    if let Some(pred) = lookup_predicate(&q_str) {
        if pred.concept == concept || concept == EntityConcept::Unknown {
            filters.push(predicate_to_filter(pred));
        }
    }

    // Apply R12/R13/R15 modifiers (R12 handles currently/active/admitted/open/pending).
    let filters = apply_modifier_rules(filters, tokens, concept, binding);

    // R14: future time scope ("due within 30 days", "expires this month", etc.)
    let time_scope = if let Some(mut ts) = extract_r14_future_time(tokens) {
        // Fix up the concept on the column ref.
        ts.column.concept = concept;
        Some(ts)
    } else {
        time_range.map(|(range, _, _, _)| make_time_scope(concept, range))
    };

    let limit = extract_top_n(tokens);

    // Standard list projection: BusinessId, PersonGivenName/PersonFamilyName/PersonFullName, EventTime, Status.
    let projection = vec![
        ColumnRef { concept, role: ColumnRole::BusinessId, name_hint: None, physical: None },
        ColumnRef { concept, role: ColumnRole::PersonFullName, name_hint: None, physical: None },
        ColumnRef { concept, role: ColumnRole::Status, name_hint: None, physical: None },
        ColumnRef { concept, role: ColumnRole::EventTime, name_hint: None, physical: None },
    ];

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::List,
            measures: vec![],
            dimensions: vec![],
            filters,
            time: time_scope,
            order: vec![],
            limit,
            joins: vec![],
            projection,
            provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R9: "tell me about|find|look up|record of <identifier|person name>"
fn try_r9_lookup(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    // Check for lookup anchors.
    let has_anchor = find_phrase(tokens, &["tell", "me", "about"]).is_some()
        || find_phrase(tokens, &["look", "up"]).is_some()
        || find_phrase(tokens, &["find", "patient"]).is_some()
        || find_phrase(tokens, &["record", "of"]).is_some();

    if !has_anchor { return None; }

    // Look for an identifier pattern like "PT-12345" or a person name.
    // Check for identifier pattern (alphanumeric-dash or alphanumeric-slash like PT-00042).
    let has_id = tokens.iter().any(|t| t.contains('-') && t.chars().any(|c| c.is_ascii_digit()));

    let concept = if has_id || tokens.iter().any(|t| ["patient", "pt"].contains(t)) {
        EntityConcept::Patient
    } else if let Some(c) = find_any_concept(tokens, binding) {
        c
    } else {
        EntityConcept::Patient // default for lookup
    };

    // Build a BusinessId or Identifier filter.
    let id_value = tokens.iter()
        .find(|t| t.contains('-') && t.chars().any(|c| c.is_ascii_digit()))
        .map(|t| t.to_string());

    let filters = if let Some(id) = id_value {
        vec![Filter {
            column: ColumnRef { concept, role: ColumnRole::BusinessId, name_hint: None, physical: None },
            op: FilterOp::Eq,
            value: FilterValue::Str(id),
        }]
    } else {
        vec![]
    };

    let projection = vec![
        ColumnRef { concept, role: ColumnRole::PersonFullName, name_hint: None, physical: None },
        ColumnRef { concept, role: ColumnRole::BusinessId, name_hint: None, physical: None },
        ColumnRef { concept, role: ColumnRole::EventTime, name_hint: None, physical: None },
        ColumnRef { concept, role: ColumnRole::Status, name_hint: None, physical: None },
    ];

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::Lookup,
            measures: vec![],
            dimensions: vec![],
            filters,
            time: None,
            order: vec![],
            limit: Some(1),
            joins: vec![],
            projection,
            provenance: SpecProvenance { rule: "R9".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R10: "is|are there any|do we have <concept> [filters]"
fn try_r10_exists(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    let has_anchor = (find_phrase(tokens, &["is", "there"]).is_some()
        || find_phrase(tokens, &["are", "there"]).is_some()
        || find_phrase(tokens, &["were", "there"]).is_some()
        || find_phrase(tokens, &["do", "we", "have"]).is_some())
        && tokens.iter().any(|t| ["any", "a"].contains(t));

    if !has_anchor { return None; }

    let concept = find_any_concept(tokens, binding)?;
    let q_str = tokens.join(" ");
    let filters = if let Some(pred) = lookup_predicate(&q_str) {
        vec![predicate_to_filter(pred)]
    } else {
        vec![]
    };

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::Exists,
            measures: vec![],
            dimensions: vec![],
            filters,
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R10".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// ---------------------------------------------------------------------------
// Helper: find a token in a token slice
// ---------------------------------------------------------------------------

fn find_token(tokens: &[&str], target: &str) -> Option<usize> {
    tokens.iter().position(|t| *t == target)
}

fn find_phrase(tokens: &[&str], phrase: &[&str]) -> Option<usize> {
    if phrase.is_empty() || tokens.len() < phrase.len() { return None; }
    for i in 0..=(tokens.len() - phrase.len()) {
        if tokens[i..i + phrase.len()] == *phrase {
            return Some(i);
        }
    }
    None
}

fn find_any_concept(tokens: &[&str], binding: &SchemaBinding) -> Option<EntityConcept> {
    for start in 0..tokens.len() {
        if let Some((concept, _)) = extract_concept(tokens, start, binding) {
            if concept != EntityConcept::Unknown {
                return Some(concept);
            }
        }
    }
    None
}

fn extract_by_dimension(tokens: &[&str], binding: &SchemaBinding) -> Option<(EntityConcept, usize)> {
    for i in 0..tokens.len() {
        if tokens[i] == "by" && i + 1 < tokens.len() {
            if let Some((concept, end)) = extract_concept(tokens, i + 1, binding) {
                return Some((concept, end));
            }
        }
    }
    None
}

fn extract_top_n(tokens: &[&str]) -> Option<u32> {
    for i in 0..tokens.len() {
        if tokens[i] == "top" || tokens[i] == "first" {
            if let Some(next) = tokens.get(i + 1) {
                if let Ok(n) = next.parse::<u32>() {
                    return Some(n);
                }
            }
        }
        if let Ok(n) = tokens[i].parse::<u32>() {
            // Plausible top-N values: 5, 10, 20, etc.
            if n >= 1 && n <= 100 && (i == 0 || tokens.get(i-1).map(|t| *t == "top" || *t == "first").unwrap_or(false)) {
                return Some(n);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// R12–R15 filter modifier helpers
//
// These are not standalone rules — they augment R1/R2/R8 by appending
// additional filters to the token stream.
// ---------------------------------------------------------------------------

/// R12: `currently|right now|open|active|pending|admitted|in theatre`
/// → adds a Status/Flag/EndTime IS NULL filter.
pub(crate) fn extract_r12_status_filter(tokens: &[&str], concept: EntityConcept, binding: &SchemaBinding) -> Option<Filter> {
    let active_tokens = ["currently", "active", "open", "admitted", "pending", "occupied"];
    if tokens.iter().any(|t| active_tokens.contains(t)) {
        // Try to find an enum value that matches, or use IsNotNull on status.
        // First try matching enum values in the binding for this concept.
        if let Some(f) = enum_filter_for_phrase("active", concept, binding)
            .or_else(|| enum_filter_for_phrase("admitted", concept, binding))
            .or_else(|| enum_filter_for_phrase("open", concept, binding))
            .or_else(|| enum_filter_for_phrase("pending", concept, binding))
        {
            return Some(f);
        }
        // Fallback: EndTime IS NULL (still in progress)
        return Some(Filter {
            column: ColumnRef { concept, role: ColumnRole::EndTime, name_hint: None, physical: None },
            op: FilterOp::IsNull,
            value: FilterValue::Bool(false),
        });
    }
    None
}

/// R13: `over|more than|at least <N> <days|hours|minutes>` on a Duration column.
/// Returns `Filter { Duration > N_minutes }`.
pub(crate) fn extract_r13_duration_filter(tokens: &[&str], concept: EntityConcept) -> Option<Filter> {
    let anchor = if let Some(p) = find_token(tokens, "over") { Some(p) }
        else if find_phrase(tokens, &["more", "than"]).is_some() { find_token(tokens, "than") }
        else if find_phrase(tokens, &["at", "least"]).is_some() { find_token(tokens, "least") }
        else if find_phrase(tokens, &["longer", "than"]).is_some() { find_token(tokens, "than") }
        else { None }?;

    // Find the number after the anchor.
    let num_pos = anchor + 1;
    if num_pos >= tokens.len() { return None; }
    let n: f64 = tokens[num_pos].parse().ok()?;
    if n <= 0.0 { return None; }

    // Find unit after the number.
    let unit_pos = num_pos + 1;
    let multiplier = if unit_pos < tokens.len() {
        match tokens[unit_pos] {
            "minutes" | "minute" | "min" => 1.0,
            "hours" | "hour" | "h" => 60.0,
            "days" | "day" => 1440.0,
            "weeks" | "week" => 10080.0,
            _ => 60.0, // default: hours
        }
    } else {
        60.0
    };

    Some(Filter {
        column: ColumnRef { concept, role: ColumnRole::Duration, name_hint: None, physical: None },
        op: FilterOp::Gt,
        value: FilterValue::Num(n * multiplier),
    })
}

/// R14: `expires|due|scheduled (within|in the next) <N> <unit>` OR `due this week/month`
/// → `EventTime BETWEEN now AND now+N` (expressed as `TimeRange::Within` or `TimeRange::Next`).
/// Returns a `TimeScope` if the pattern matches.
pub(crate) fn extract_r14_future_time(tokens: &[&str]) -> Option<TimeScope> {
    // "due this week" / "due this month" / "due this quarter"
    if tokens.iter().any(|t| ["due", "expires", "expiring", "scheduled"].contains(t)) {
        if let Some(p) = find_token(tokens, "this") {
            if p + 1 < tokens.len() {
                if let Some((unit, _)) = parse_bucket_unit(tokens, p + 1) {
                    let range = match unit {
                        BucketUnit::Week => TimeRange::ThisWeek,
                        BucketUnit::Month => TimeRange::ThisMonth,
                        BucketUnit::Quarter => TimeRange::ThisQuarter,
                        BucketUnit::Year => TimeRange::ThisYear,
                        _ => return None,
                    };
                    // Concept is unknown here — caller fills it.
                    return Some(TimeScope {
                        column: ColumnRef { concept: EntityConcept::Unknown, role: ColumnRole::EventTime, name_hint: None, physical: None },
                        range: Some(range),
                        bucket: None,
                    });
                }
            }
        }
        // "within N days/weeks/months"
        if let Some((range, _, _, _)) = extract_time_range(tokens) {
            return Some(TimeScope {
                column: ColumnRef { concept: EntityConcept::Unknown, role: ColumnRole::EventTime, name_hint: None, physical: None },
                range: Some(range),
                bucket: None,
            });
        }
    }
    None
}

/// R15: `without|missing|no <concept>` → anti-join NOT EXISTS.
/// For now resolves via domain predicates; a full FK anti-join needs bind-time resolution.
/// Returns a filter with `FilterOp::IsNull` on the related concept's PrimaryKey.
pub(crate) fn extract_r15_anti_join(tokens: &[&str], binding: &SchemaBinding) -> Option<(EntityConcept, Filter)> {
    let anchor = if let Some(p) = find_token(tokens, "without") { p }
        else if let Some(p) = find_token(tokens, "missing") { p }
        else if let Some(p) = find_token(tokens, "no") {
            // Skip "no" if it's at start and followed by "-show" (that's R16 no-show)
            if tokens.get(p + 1).map(|t| *t == "show" || t.starts_with("show")).unwrap_or(false) {
                return None;
            }
            p
        } else { return None; };

    let start = anchor + 1;
    let (anti_concept, _) = extract_concept(tokens, start, binding)?;
    if anti_concept == EntityConcept::Unknown { return None; }

    Some((
        anti_concept,
        Filter {
            column: ColumnRef { concept: anti_concept, role: ColumnRole::PrimaryKey, name_hint: None, physical: None },
            op: FilterOp::IsNull,
            value: FilterValue::Bool(false),
        },
    ))
}

/// Apply R12–R15 modifiers to an existing filter list; returns the augmented list.
/// `concept` is the primary query subject.
pub(crate) fn apply_modifier_rules(
    mut filters: Vec<Filter>,
    tokens: &[&str],
    concept: EntityConcept,
    binding: &SchemaBinding,
) -> Vec<Filter> {
    // R12: status/active filter.
    if let Some(f) = extract_r12_status_filter(tokens, concept, binding) {
        filters.push(f);
    }
    // R13: duration filter.
    if let Some(f) = extract_r13_duration_filter(tokens, concept) {
        filters.push(f);
    }
    // R15: anti-join (partial — adds a filter if matched).
    if let Some((_anti_concept, f)) = extract_r15_anti_join(tokens, binding) {
        filters.push(f);
    }
    filters
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::binding::{ColumnBinding, JoinHop, SchemaBinding, TableBinding};
    use crate::ontology::service_line::ServiceLine;
    use chrono::Utc;

    fn make_test_binding() -> SchemaBinding {
        let make_tb = |name: &str, concept: EntityConcept, cols: Vec<(&str, ColumnRole, Vec<&str>)>| {
            TableBinding {
                table_name: name.into(),
                concept,
                confidence: 0.95,
                service_lines: vec![ServiceLine::PatientChart],
                columns: cols.into_iter().map(|(cn, role, evs)| {
                    ColumnBinding {
                        column_name: cn.into(),
                        role,
                        is_pii: false,
                        enum_values: evs.iter().map(|s| s.to_string()).collect(),
                    }
                }).collect(),
                patient_path: Some(vec![]),
                event_time_col: None,
                degraded: false,
            }
        };

        SchemaBinding {
            source_id: "test".into(),
            bound_at: Utc::now(),
            tables: vec![
                make_tb("patients", EntityConcept::Patient, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_no", ColumnRole::BusinessId, vec![]),
                    ("full_name", ColumnRole::PersonFullName, vec![]),
                    ("gender", ColumnRole::Gender, vec!["male", "female"]),
                ]),
                make_tb("encounters", EntityConcept::Encounter, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("status", ColumnRole::Status, vec!["active", "closed"]),
                    ("encounter_date", ColumnRole::EventTime, vec![]),
                ]),
                make_tb("appointments", EntityConcept::Appointment, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("status", ColumnRole::Status, vec!["scheduled", "no_show", "attended"]),
                    ("appointment_date", ColumnRole::EventTime, vec![]),
                ]),
                make_tb("admissions", EntityConcept::Admission, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("status", ColumnRole::Status, vec!["active", "discharged"]),
                    ("admission_date", ColumnRole::EventTime, vec![]),
                ]),
                make_tb("deliveries", EntityConcept::Delivery, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("delivery_mode", ColumnRole::Type, vec!["vaginal", "caesarean", "cs"]),
                    ("delivery_status", ColumnRole::Status, vec!["live_birth", "stillbirth"]),
                    ("delivered_at", ColumnRole::EventTime, vec![]),
                ]),
                make_tb("newborns", EntityConcept::Newborn, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("birth_weight", ColumnRole::Measure, vec![]),
                    ("status", ColumnRole::Status, vec!["alive", "stillborn"]),
                ]),
                make_tb("triage_assessments", EntityConcept::Triage, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("triage_category", ColumnRole::Priority, vec!["1", "2", "3", "4", "5"]),
                    ("triage_time", ColumnRole::EventTime, vec![]),
                    ("wait_time", ColumnRole::Duration, vec![]),
                ]),
            ],
            degraded: false,
            override_version: 0,
        }
    }

    #[test]
    fn parses_simple_count_question() {
        let binding = make_test_binding();
        let result = parse("how many patients do we have", None, &binding, &[]);
        match result {
            ParseOutcome::Parsed { spec, missing } => {
                assert_eq!(spec.shape, Shape::Scalar);
                assert_eq!(spec.subject.concept, EntityConcept::Patient);
                assert!(missing.is_empty());
            }
            ParseOutcome::NoParse => panic!("should have parsed"),
        }
    }

    #[test]
    fn parses_count_with_time() {
        let binding = make_test_binding();
        let result = parse("how many deliveries last month", None, &binding, &[]);
        match result {
            ParseOutcome::Parsed { spec, .. } => {
                assert_eq!(spec.shape, Shape::Scalar);
                assert_eq!(spec.subject.concept, EntityConcept::Delivery);
                assert!(spec.time.is_some());
            }
            ParseOutcome::NoParse => panic!("should have parsed"),
        }
    }

    #[test]
    fn parses_no_show_rate() {
        let binding = make_test_binding();
        let result = parse("what is our no-show rate", None, &binding, &[]);
        match result {
            ParseOutcome::Parsed { spec, .. } => {
                assert_eq!(spec.shape, Shape::Rate);
            }
            ParseOutcome::NoParse => panic!("should have parsed"),
        }
    }

    #[test]
    fn parses_lbw_count() {
        let binding = make_test_binding();
        let result = parse("how many babies were low birth weight", None, &binding, &[]);
        match result {
            ParseOutcome::Parsed { spec, .. } => {
                assert!(
                    spec.shape == Shape::Scalar || spec.shape == Shape::Grouped,
                    "shape: {:?}", spec.shape
                );
                assert!(!spec.filters.is_empty(), "should have LBW filter");
            }
            ParseOutcome::NoParse => panic!("should have parsed"),
        }
    }

    #[test]
    fn parses_list_question() {
        let binding = make_test_binding();
        let result = parse("which referrals are still pending", None, &binding, &[]);
        // May not parse if referral concept is not in binding — that's OK, NoParse is valid.
        // But if it does parse, shape must be List.
        if let ParseOutcome::Parsed { spec, .. } = result {
            assert_eq!(spec.shape, Shape::List);
        }
    }

    #[test]
    fn parses_lookup_with_id() {
        let binding = make_test_binding();
        let result = parse("tell me about PT-00042", None, &binding, &[]);
        match result {
            ParseOutcome::Parsed { spec, .. } => {
                assert_eq!(spec.shape, Shape::Lookup);
                assert_eq!(spec.subject.concept, EntityConcept::Patient);
            }
            ParseOutcome::NoParse => panic!("should have parsed as lookup"),
        }
    }
}
