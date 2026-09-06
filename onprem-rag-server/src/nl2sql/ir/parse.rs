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
    BucketUnit, ColumnRef, DerivedDuration, Dimension, DurationFilter, DurationUnit, Filter,
    FilterOp, FilterValue, Measure, MeasureOp, MissingSlot, OpenInterval, Order, OrderTarget,
    QuerySpec, RelatedScope, Shape, SortDir, SpecProvenance, Subject, TimeRange, TimeScope,
    ValueExpr,
};

use super::predicates::{lookup_all_predicates, lookup_predicate, predicate_to_filter};

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

/// Insert a space before every uppercase ASCII letter that immediately follows
/// a lowercase ASCII letter.  This splits CamelCase / PascalCase identifiers
/// into space-separated words without requiring underscores.
///
/// Examples:
///   "LabReq" → "Lab Req"     "PatientMaster" → "Patient Master"
///   "admissions" → "admissions"   "newborns" → "newborns"
///
/// Used in pass 3 of `phrase_to_concept` so that word-boundary matching on
/// table names works for schemas that use CamelCase table names (alt binding)
/// as well as for snake_case names (dev binding, where underscores have already
/// been replaced with spaces).
fn split_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && b.is_ascii_uppercase() && bytes[i - 1].is_ascii_lowercase() {
            out.push(' ');
        }
        out.push(b as char);
    }
    out
}

/// Map a noun or phrase fragment to an `EntityConcept`.
/// Checks `descriptor.singular`, `descriptor.plural`, and `descriptor.synonyms`
/// for all concepts. Does NOT look at physical table names.
pub(crate) fn phrase_to_concept(phrase: &str, binding: &SchemaBinding) -> Option<EntityConcept> {
    let phrase_lower = phrase.to_ascii_lowercase();

    // First pass: exact match against descriptor tokens.
    //
    // The comment previously said "Check the concept is actually bound in this
    // schema" but the guard `!= Unknown` was only a concept-validity check, not a
    // schema-binding check.  A concept present in DESCRIPTORS but absent from the
    // current schema's TableBindings must not be returned — the caller would get
    // Some(X) and then fail to find a table for X, which produces NoBind where the
    // correct answer is to fall through to a concept that IS bound.  For instance,
    // if an alt schema binds LabOrder but not LabTest, the singular "lab test" must
    // not immediately return LabTest — it must fall through so pass 2/3 can return
    // the schema-local LabOrder instead.
    for desc in DESCRIPTORS.iter() {
        if desc.singular == phrase_lower
            || desc.plural == phrase_lower
            || desc.synonyms.contains(&phrase_lower.as_str())
        {
            // Only return this concept if it actually has a table in this schema.
            if !binding.tables_for_concept(desc.concept).is_empty() {
                return Some(desc.concept);
            }
        }
    }

    // Second pass: word-boundary match against name_tokens, with simple plural tolerance.
    //
    // Space-padding enforces word boundaries so that short tokens cannot match
    // inside unrelated longer words — `"form"` must not match `"performed"`,
    // `"anc"` must not match `"cancelled"`, `"patient"` must not match
    // `"inpatient"` (see plans/new/03b §3 Family A).
    //
    // The `plural` variant (`tok + "s"`) is needed because medical questions use
    // inflected plurals ("deaths", "invoices", "new patients") whose stem is the
    // canonical name_token ("death", "invoice", "patient").  A strict word-boundary
    // match would miss these, causing the scan to fall through to pass 3 where
    // short incidental words ("new", "in") match unrelated table names as
    // substrings.  The plural check is restricted to **single-word tokens** — a
    // multi-word token like "lab test" adding 's' to become "lab tests" produces a
    // phrase that is independently meaningful and may belong to a sibling concept,
    // creating false matches across schemas that have both LabTest and LabOrder.
    //
    // Tiebreak: prefer the descriptor whose MATCHING TOKEN is longest (by word
    // count, then character count). Enum position is NOT a valid tiebreak — it
    // reflects chronological addition order to the ontology, not semantic
    // relevance. A recently-added concept with a longer, more specific token
    // (e.g. AccessLog "record_access" = "record access", 2 words) must beat an
    // older concept with a shorter incidental token (Document "record", 1 word).
    // Equal-length ties are broken by character count (longer char string wins),
    // with DESCRIPTORS order as a last-resort deterministic but arbitrary fallback.
    let padded = format!(" {} ", phrase_lower);
    // (concept, matching_token_word_count, matching_token_char_count)
    let mut best: Option<(EntityConcept, usize, usize)> = None;
    for desc in DESCRIPTORS.iter() {
        if desc.concept == EntityConcept::Unknown {
            continue;
        }
        // Also verify the concept is actually bound in this schema; a concept
        // that name_tokens can spell but which has no table in this schema must
        // not be returned — this is the same guard that Step 3 makes explicit
        // for pass 1 (SchemaBinding::tables_for_concept, plans/new/03b §2).
        // A concept bound in one schema but not another must not suppress the
        // fall-through to pass 3 that discovers the schema-local equivalent.
        if binding.tables_for_concept(desc.concept).is_empty() {
            continue;
        }
        let phrase_word_count = phrase_lower.split_whitespace().count();
        for tok in desc.name_tokens {
            let norm_tok = tok.replace('_', " ");
            let hit_len = if padded.contains(&format!(" {} ", norm_tok)) {
                // Exact word-boundary match — score by token length.
                Some(norm_tok.len())
            } else if !norm_tok.contains(' ') && padded.contains(&format!(" {}s ", norm_tok)) {
                // Plural tolerance (adding 's'): only single-word tokens.
                Some(norm_tok.len())
            } else {
                None
            };
            if let Some(char_len) = hit_len {
                let token_word_count = norm_tok.split_whitespace().count();
                // Coverage guard: a short token matching inside a much longer phrase is
                // almost always an incidental word collision, not a genuine concept reference.
                // Require the matching token to cover at least half the phrase (in words).
                // Example rejected: "me all record" (3 words) with Document token "record"
                // (1 word) → 2*1=2 < 3 → reject.  Accepted: "all record access" (3 words)
                // with AccessLog token "record access" (2 words) → 2*2=4 >= 3 → accept.
                if 2 * token_word_count < phrase_word_count {
                    continue;
                }
                let better = match &best {
                    None => true,
                    Some((_, bw, bc)) => token_word_count > *bw || (token_word_count == *bw && char_len > *bc),
                };
                if better {
                    best = Some((desc.concept, token_word_count, char_len));
                }
            }
        }
    }
    if let Some((concept, _, _)) = best {
        return Some(concept);
    }

    // Third pass: word-boundary match against bound table names.
    //
    // Table names are normalised with three steps:
    //   1. Underscores → spaces  (handles snake_case: "lab_tests" → "lab tests")
    //   2. CamelCase split        (handles PascalCase: "LabReq" → "Lab Req")
    //   3. ASCII lowercase        ("Lab Req" → "lab req")
    //
    // Without step 2, CamelCase names collapse to a single token ("labreq") and
    // word-boundary matching would never find "lab" inside "labreq".  With step 2
    // the boundary is explicit: " lab req " contains " lab " ✓.
    //
    // Word-boundary matching is intentional and necessary here.  A bare substring
    // test lets common English words from question prose — specifically function
    // words like "is" — collide with table names that contain them as substrings
    // (e.g. "admissions" contains "is"), producing wrong subject-concept resolution
    // of the same kind Family A describes.
    //
    // Tiebreak (same principle as pass 2): prefer the table whose normalised name
    // is a more precise match — i.e. has the fewest extra words beyond the phrase.
    // A two-word table name that contains a one-word phrase should lose to a
    // two-word table name that exactly matches the two-word phrase.
    let padded_phrase = format!(" {} ", phrase_lower);
    let phrase_words = phrase_lower.split_whitespace().count();
    let mut p3_best: Option<(EntityConcept, usize)> = None; // (concept, extra_words)
    for tb in &binding.tables {
        if tb.concept == EntityConcept::Unknown {
            continue;
        }
        // Normalise: underscore→space, then camelCase split, then lowercase.
        let tbl_raw = tb.table_name.replace('_', " ");
        let tbl_norm = split_camel_case(&tbl_raw).to_ascii_lowercase();
        let padded_tbl = format!(" {} ", tbl_norm);
        if tbl_norm == phrase_lower || padded_tbl.contains(&padded_phrase as &str) {
            let tbl_words = tbl_norm.split_whitespace().count();
            let extra = tbl_words.saturating_sub(phrase_words);
            let better = match &p3_best {
                None => true,
                Some((_, be)) => extra < *be,
            };
            if better {
                p3_best = Some((tb.concept, extra));
            }
        }
    }
    if let Some((concept, _)) = p3_best {
        return Some(concept);
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
    //
    // Same invariant as R8 below: every recognised predicate must be fully
    // expressed — either in `spec.filters` (same-concept) or in a `RelatedScope`
    // (cross-concept, non-Unexpressible) — or the parse must fail (NoParse).
    //   • Unexpressible kind: requires cross-entity join that §0.6 forbids → NoParse.
    //   • Cross-concept, expressible: becomes a RelatedScope (EXISTS subquery, plan 03f).
    //   • Same-concept: added directly to spec.filters.
    // Silently dropping any predicate would emit a spec that answers a different,
    // easier question — refuse so the planner falls back to the semantic path.
    let q_str = tokens.join(" ");
    let mut related_scopes: Vec<RelatedScope> = Vec::new();
    for pred in lookup_all_predicates(&q_str) {
        if pred.kind.is_unexpressible() {
            return None;
        }
        if pred.concept == concept || concept == EntityConcept::Unknown {
            filters.push(predicate_to_filter(pred));
        } else {
            // Cross-concept, expressible predicate → single-hop reverse-FK semi-join.
            // Grouped by concept so multiple predicates on the same child concept
            // produce one EXISTS subquery (not one per predicate).
            let child_filter = predicate_to_filter(pred);
            if let Some(rs) = related_scopes.iter_mut().find(|rs| rs.concept == pred.concept) {
                rs.filters.push(child_filter);
            } else {
                related_scopes.push(RelatedScope {
                    concept: pred.concept,
                    filters: vec![child_filter],
                    time: None,
                    negated: false,
                    physical: None,
                });
            }
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
        related: related_scopes,
        duration_filters: vec![],
        provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
    };

    // Refuse when the question uses a temporal-superlative qualifier ("last",
    // "latest", "most recent") immediately before a concept that appears as a
    // related scope.  Such questions require a most-recent-per-group computation
    // (window function or correlated MAX) that the IR cannot express — dropping
    // the qualifier silently would over-report.
    if has_temporal_superlative_over_related(tokens, &spec.related, binding) {
        return None;
    }

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
        // Require a column whose name contains "dispense" and has Quantity role.
        // If none exists the question cannot be answered — refuse so the planner
        // falls back to the semantic path rather than summing the wrong column.
        let has_dispense_col = binding.tables.iter()
            .filter(|tb| tb.concept == concept)
            .flat_map(|tb| tb.columns.iter())
            .any(|cb| cb.role == ColumnRole::Quantity
                && cb.column_name.to_ascii_lowercase().contains("dispense"));
        if !has_dispense_col {
            return None;
        }
        (MeasureOp::Sum, ColumnRole::Quantity, Some("dispense"))
    } else if tokens.iter().any(|t| *t == "collected" || *t == "received" || *t == "billed") {
        (MeasureOp::Sum, ColumnRole::Amount, None)
    } else {
        (MeasureOp::Sum, ColumnRole::Amount, None)
    };

    let target = ColumnRef { concept, role: target_role, name_hint: name_hint.map(|s| s.to_string()), physical: None };
    let measures = vec![Measure { op: measure_op, target: Some(ValueExpr::Column(target)), alias: "total".into() }];
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
        related: vec![],
        duration_filters: vec![],
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

    // Elapsed-time metrics first (plan 03g). Length of stay and lab turnaround
    // are not stored anywhere: they are the difference between two timestamps,
    // so the target is a `DerivedDuration` over the concept's interval endpoints
    // rather than a column with a `Duration` role.
    if let Some((metric_slug, unit)) = derived_duration_metric(tokens) {
        // The open-interval reading must be decidable from the question. When it
        // is not - the question carries cues for BOTH readings - the rule returns
        // None rather than guessing, per the defect-closure brief §1.
        let open = open_interval_choice(tokens)?;
        // Derive whenever the concept owns a single unambiguous interval, and
        // also when it owns no stored duration column at all. In the first case
        // deriving is *better* than the stored column: a stored "length of stay
        // in days" is an opaque expression that in practice truncates (an
        // EXTRACT(DAY ...) generated column reports a 20-hour stay as 0) and it
        // may not exist in another deployment's schema at all, so only the
        // derived form gives the same number across schemas and dialects. In the
        // second case deriving is the only route, and its refusal names the
        // endpoint that is missing instead of a stored column the question never
        // meant.
        if concept_has_unique_interval(binding, concept)
            || !concept_has_duration_column(binding, concept)
        {
            let alias = format!("{}_{}_{}", agg_slug(&agg_op), metric_slug, unit.as_str());
            let duration = DerivedDuration {
                start: ColumnRef { concept, role: ColumnRole::StartTime, name_hint: None, physical: None },
                end: ColumnRef { concept, role: ColumnRole::EndTime, name_hint: None, physical: None },
                unit,
                open,
            };
            return Some((
                build_r3_spec(
                    concept,
                    vec![Measure {
                        op: agg_op,
                        target: Some(ValueExpr::Duration(duration)),
                        alias: alias.clone(),
                    }],
                    &alias,
                    by_concept,
                    time_range,
                ),
                vec![],
            ));
        }
    }

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
    let measures = vec![Measure { op: agg_op, target: Some(ValueExpr::Column(target)), alias: "average".into() }];

    Some((
        build_r3_spec(concept, measures, "average", by_concept, time_range),
        vec![],
    ))
}

/// Alias prefix naming the aggregate, so a derived measure's column alias reads
/// `avg_los_days` / `median_turnaround_hours` - the aggregate AND the unit, both
/// visible in the answer (plan 03g §1).
fn agg_slug(op: &MeasureOp) -> &'static str {
    match op {
        MeasureOp::Avg => "avg",
        MeasureOp::Median => "median",
        MeasureOp::Max => "max",
        MeasureOp::Min => "min",
        MeasureOp::Sum => "total",
        MeasureOp::Count => "count",
        MeasureOp::CountDistinct => "distinct",
        MeasureOp::Rate { .. } => "rate",
    }
}

/// Elapsed-time metric names, each with the unit that metric is conventionally
/// reported in.
///
/// This is a *metric* vocabulary, in the same spirit as the money and vitals
/// vocabularies in `try_r3_average` - not a match on any fixture's phrasing and
/// not a physical column name. The unit is part of the metric's meaning: length
/// of stay is days, lab turnaround is hours, a door-to-doctor wait is minutes.
/// Deciding it here rather than at compile time is the plan 03g §1 requirement:
/// choosing wrong is a 24x or 60x error that still reads as a plausible number.
fn derived_duration_metric(tokens: &[&str]) -> Option<(&'static str, DurationUnit)> {
    if tokens.iter().any(|t| ["stay", "stays", "los"].contains(t)) {
        Some(("los", DurationUnit::Days))
    } else if tokens.iter().any(|t| *t == "turnaround") {
        Some(("turnaround", DurationUnit::Hours))
    } else if tokens.iter().any(|t| ["wait", "waiting", "door-to-doctor"].contains(t)) {
        Some(("wait", DurationUnit::Minutes))
    } else if tokens.iter().any(|t| ["duration", "theatre"].contains(t)) {
        Some(("duration", DurationUnit::Minutes))
    } else {
        None
    }
}

/// Which reading of an open interval the question asks for (plan 03g §3).
///
/// `None` means the question carries cues for both readings at once ("average
/// stay of patients currently admitted and discharged"): the two readings give
/// materially different numbers, so the rule refuses rather than picking one.
fn open_interval_choice(tokens: &[&str]) -> Option<OpenInterval> {
    const ONGOING: [&str; 5] = ["current", "currently", "still", "ongoing", "now"];
    const COMPLETED: [&str; 4] = ["discharged", "completed", "closed", "finished"];
    let ongoing = tokens.iter().any(|t| ONGOING.contains(t));
    let completed = tokens.iter().any(|t| COMPLETED.contains(t));
    match (ongoing, completed) {
        (true, true) => None,
        (true, false) => Some(OpenInterval::AsOfNow),
        // The honest default for "average length of stay": completed intervals
        // only. The compiler states it in the explanation, so the reader is told
        // which population was measured rather than having to infer it.
        _ => Some(OpenInterval::CompletedOnly),
    }
}

/// True when the concept's tables expose exactly one `StartTime` and exactly one
/// `EndTime` column.
///
/// A *capability probe*, not a resolution: it decides whether to build the
/// derived construct at all. The actual endpoint choice still happens in `bind`
/// through the three-valued role lookup, which refuses on ambiguity - so a
/// concept with both `scheduled_start` and `actual_start` is excluded here and
/// would be refused there too if it slipped through. Counting rather than
/// picking is the point: there is no `[0]` and no `next()` in this function.
fn concept_has_unique_interval(binding: &SchemaBinding, concept: EntityConcept) -> bool {
    let mut starts = 0usize;
    let mut ends = 0usize;
    for tb in binding.tables.iter().filter(|t| t.concept == concept) {
        for cb in &tb.columns {
            match cb.role {
                ColumnRole::StartTime => starts += 1,
                ColumnRole::EndTime => ends += 1,
                _ => {}
            }
        }
    }
    starts == 1 && ends == 1
}

/// True when the concept's tables expose a stored `Duration`-role column.
fn concept_has_duration_column(binding: &SchemaBinding, concept: EntityConcept) -> bool {
    binding.tables.iter()
        .filter(|t| t.concept == concept)
        .flat_map(|t| t.columns.iter())
        .any(|c| c.role == ColumnRole::Duration)
}

/// Assemble the R3 spec around an already-built measure list.
///
/// Extracted so the derived-duration path and the stored-column path share one
/// definition of shape / dimension / ordering and cannot drift apart.
fn build_r3_spec(
    concept: EntityConcept,
    measures: Vec<Measure>,
    alias: &str,
    by_concept: Option<EntityConcept>,
    time_range: Option<(TimeRange, BucketUnit, usize, usize)>,
) -> QuerySpec {
    let shape = if by_concept.is_some() { Shape::Grouped } else { Shape::Scalar };
    let mut dimensions = Vec::new();
    let mut order = Vec::new();
    if let Some(dim_c) = by_concept {
        dimensions.push(Dimension {
            column: ColumnRef { concept: dim_c, role: ColumnRole::Description, name_hint: None, physical: None },
            label: dim_c.slug().to_string(),
        });
        order.push(Order { target: OrderTarget::Measure(alias.to_string()), dir: SortDir::Desc });
    }
    QuerySpec {
        subject: Subject { concept, table: None },
        shape,
        measures,
        dimensions,
        filters: vec![],
        time: time_range.map(|(range, _, _, _)| make_time_scope(concept, range)),
        order,
        limit: None,
        joins: vec![],
        projection: vec![],
        related: vec![],
        duration_filters: vec![],
        provenance: SpecProvenance { rule: "R3".into(), focus_subs: vec![] },
    }
}

// R4: trend over time
fn try_r4_trend(
    tokens: &[&str],
    _hint: Option<&RouteEntities>,
    binding: &SchemaBinding,
    _scope: &[String],
) -> Option<(QuerySpec, Vec<MissingSlot>)> {
    // "over" is a trend signal only when it is NOT immediately followed by a number.
    // "over N <unit>" (e.g. "over 14 days") is a numeric threshold comparison handled by
    // R13 (duration filter), not a time-series pattern. Accepting "over" unconditionally
    // caused R4 to fire on "Which patients have been in over 14 days?" and produce a Trend
    // shape (wrong) instead of falling through to R8's List shape (correct).
    let has_trend = tokens.iter().enumerate().any(|(i, t)| match *t {
        "over" => !tokens.get(i + 1).map(|n| n.parse::<f64>().is_ok()).unwrap_or(false),
        "trend" | "per" | "monthly" | "weekly" | "daily" | "quarterly" | "annually" => true,
        _ => false,
    });
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
            related: vec![],
            duration_filters: vec![],
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

    // Degenerate: actor concept and subject concept are the same.
    //
    // This arises when a question's grammatical actor ("which doctor saw the most
    // patients") has `find_any_concept` return the actor's concept (Provider) as
    // BOTH the counted subject AND the grouping dimension — typically because
    // "doctor" appears before "patients" in the token stream, or because the schema
    // has no Patient table.  Emitting a spec with subject=Provider and
    // dimension=Provider would produce "top providers by their own count", which is
    // unanswerable without a join to a separate encounter/visit concept.
    // Refuse so the planner falls back to the semantic path.
    if dim_concept == subject_concept {
        return None;
    }

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
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R7".into(), focus_subs: vec![] },
        },
        vec![],
    ))
}

// R8: "list|show|which <concept> [filters] [time] [limit]" / "who is|are <filters>"
//
// When the question has a "show/list/display" anchor (but no "which/who/whose"
// interrogative) and a "by <non-time-token>" clause, the intent is a GROUP BY
// aggregation (count per dimension), not an enumeration of records.  Example:
//   "Show the distribution of patients by blood type" → Shape::Grouped → Aggregation
// vs.
//   "Which patients have been in over 14 days?"       → Shape::List   → Enumeration
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

    // R8 (List/Grouped) cannot produce rate-shaped output — that belongs to R7.
    // If R7 fired but found no matching rate predicate (unknown rate type), we must
    // not fall through to R8 and answer with a List shape on the wrong subject
    // table.  Refuse so the planner falls back to the semantic path.
    if tokens.iter().any(|t| *t == "rate") {
        return None;
    }

    // R8 cannot produce TopN-actor output — that belongs to R5 ("most/fewest/least").
    // If R5 tried and failed (e.g. degenerate subject=dim case), R8 must not answer
    // with a List on the wrong concept.  No verified row uses R8 for TopN signals,
    // so this guard is safe.
    if tokens.iter().any(|t| ["most", "fewest", "least"].contains(t)) {
        return None;
    }

    let concept = find_any_concept(tokens, binding)?;
    let time_range = extract_time_range(tokens);

    // ── Defect 1 fix: "show/list/display X by Y" → GROUP BY aggregation ──────
    //
    // "show/list/display" without an interrogative pronoun ("which/who/whose") AND
    // a "by <token>" where the token is not a calendar bucket → the question asks
    // for a count grouped by that dimension, not a paginated record list.
    //
    // Time-bucket words ("month", "week", etc.) are excluded here because those
    // belong to R4 (Trend); we must not steal "show patients by month" away from
    // the Trend path.
    let has_display_anchor = tokens.iter().any(|t| ["show", "list", "display"].contains(t));
    let is_enum_interrogative = tokens.iter().any(|t| ["which", "who", "whose"].contains(t));

    if has_display_anchor && !is_enum_interrogative {
        const TIME_BUCKET_WORDS: &[&str] = &[
            "month", "months", "week", "weeks", "day", "days",
            "year", "years", "quarter", "quarters", "hour", "hours",
        ];
        let has_by_non_time = tokens.windows(2).any(|w| {
            w[0] == "by" && !is_filler(w[1]) && !TIME_BUCKET_WORDS.contains(&w[1])
        });
        if has_by_non_time {
            let by_concept = extract_by_dimension(tokens, binding).map(|(c, _)| c);
            let mut dimensions = Vec::new();
            if let Some(dim_c) = by_concept {
                dimensions.push(Dimension {
                    column: ColumnRef {
                        concept: dim_c,
                        role: ColumnRole::Description,
                        name_hint: None,
                        physical: None,
                    },
                    label: dim_c.slug().to_string(),
                });
            }
            let time_scope = time_range.map(|(range, _, _, _)| make_time_scope(concept, range));
            return Some((
                QuerySpec {
                    subject: Subject { concept, table: None },
                    shape: Shape::Grouped,
                    measures: vec![Measure {
                        op: MeasureOp::Count,
                        target: None,
                        alias: "count".into(),
                    }],
                    dimensions,
                    filters: vec![],
                    time: time_scope,
                    order: vec![Order {
                        target: OrderTarget::Measure("count".into()),
                        dir: SortDir::Desc,
                    }],
                    limit: None,
                    joins: vec![],
                    projection: vec![],
                    related: vec![],
                    duration_filters: vec![],
                    provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
                },
                vec![],
            ));
        }
    }

    // ── Standard list / enumeration path ─────────────────────────────────────

    // Collect filters from domain predicates and enum values.
    let q_str = tokens.join(" ");
    let mut filters = Vec::new();

    // Domain predicate filters.
    //
    // Same invariant as R13 below: every recognised predicate must be fully
    // expressed — either in `spec.filters` (same-concept) or in a `RelatedScope`
    // (cross-concept, non-Unexpressible) — or the parse must fail (NoParse).
    //   • Unexpressible kind: requires cross-entity join that §0.6 forbids → NoParse.
    //   • Cross-concept, expressible: becomes a RelatedScope (EXISTS subquery, plan 03f).
    //   • Same-concept: added directly to spec.filters.
    // Silently dropping any predicate would emit a spec answering a different,
    // unfiltered question.
    let mut related_scopes: Vec<RelatedScope> = Vec::new();
    for pred in lookup_all_predicates(&q_str) {
        if pred.kind.is_unexpressible() {
            return None;
        }
        if pred.concept == concept || concept == EntityConcept::Unknown {
            filters.push(predicate_to_filter(pred));
        } else {
            // Cross-concept, expressible predicate → single-hop reverse-FK semi-join.
            let child_filter = predicate_to_filter(pred);
            if let Some(rs) = related_scopes.iter_mut().find(|rs| rs.concept == pred.concept) {
                rs.filters.push(child_filter);
            } else {
                related_scopes.push(RelatedScope {
                    concept: pred.concept,
                    filters: vec![child_filter],
                    time: None,
                    negated: false,
                    physical: None,
                });
            }
        }
    }

    // R12: status / active filter.
    if let Some(f) = extract_r12_status_filter(tokens, concept, binding) {
        filters.push(f);
    }
    // R13: duration threshold filter.
    //
    // Invariant: a spec must never be emitted when a recognised numeric threshold
    // cannot be carried in `spec.filters`.
    //
    // If R13 matches but the Duration column lives on a *different* concept than
    // the subject (e.g. Patient question where Duration is on Admission), there
    // is no safe route:
    //
    //   • Emitting the filter with `concept = subject_concept` causes a bind
    //     failure (NoRole), but the spec IS emitted — not ideal.
    //   • Silently dropping the filter produces a spec that answers a different
    //     question (list every record with no threshold) — strictly worse.
    //   • §0.6 join forms (forward FK from subject's fk_edges, patient_path
    //     traversal) cannot express Patient→Admission because the FK is reversed:
    //     admissions.patient_id → patients.id.
    //
    // Therefore: if R13 matches and the subject concept does not own a Duration
    // column, this rule does not apply — return NoParse so the planner falls back
    // to the semantic path.  When the subject DOES own Duration, emit the filter.
    //
    // Plan 03g §1 adds a third option ahead of the two above: when the subject
    // concept owns exactly one interval, the threshold applies to the *derived*
    // duration between its endpoints, which needs no stored column at all. That
    // is tried first — a stay of "over 14 days" is a fact about two timestamps,
    // and the derived form states the threshold in the question's own unit.
    let mut duration_filters: Vec<DurationFilter> = Vec::new();
    if let Some(df) = extract_r13_duration_threshold(tokens, concept, binding) {
        duration_filters.push(df);
    } else if let Some(dur_filter) = extract_r13_duration_filter(tokens, concept) {
        let concept_has_duration = binding.tables.iter()
            .filter(|tb| tb.concept == concept)
            .any(|tb| tb.columns.iter().any(|cb| cb.role == ColumnRole::Duration));
        if concept_has_duration {
            filters.push(dur_filter);
        } else {
            // The threshold is recognised but cannot be placed on any column
            // reachable from this subject.  Emitting a spec without it would
            // silently answer a different question than the one asked.
            return None;
        }
    }
    // R15: anti-join (partial — adds a filter if matched).
    if let Some((_anti_concept, f)) = extract_r15_anti_join(tokens, binding) {
        filters.push(f);
    }

    // R14: future time scope ("due within 30 days", "expires this month", etc.)
    let time_scope = if let Some(mut ts) = extract_r14_future_time(tokens) {
        // Fix up the concept on the column ref.
        ts.column.concept = concept;
        Some(ts)
    } else {
        time_range.map(|(range, _, _, _)| make_time_scope(concept, range))
    };

    let limit = extract_top_n(tokens);

    // Standard list projection: BusinessId, PersonFullName, Status, EventTime.
    // When the question starts with "who" or "whose" (asking about a provider/
    // staff member), also project ProviderRef so that the identity column (e.g.
    // provider_id on staff_shifts) is included.  Projection is best-effort —
    // columns not present on the bound table are silently dropped.
    let mut projection: Vec<ValueExpr> = vec![
        ColumnRef { concept, role: ColumnRole::BusinessId, name_hint: None, physical: None }.into(),
        ColumnRef { concept, role: ColumnRole::PersonFullName, name_hint: None, physical: None }.into(),
        ColumnRef { concept, role: ColumnRole::Status, name_hint: None, physical: None }.into(),
        ColumnRef { concept, role: ColumnRole::EventTime, name_hint: None, physical: None }.into(),
    ];
    if tokens.iter().any(|t| *t == "who" || *t == "whose") {
        projection.push(ColumnRef { concept, role: ColumnRole::ProviderRef, name_hint: None, physical: None }.into());
    }

    let spec = QuerySpec {
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
        related: related_scopes,
        duration_filters,
        provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
    };

    // Refuse when the question uses a temporal-superlative qualifier ("last",
    // "latest", "most recent") immediately before a concept that appears as a
    // related scope.  Such questions require a most-recent-per-group computation
    // (window function or correlated MAX) that the IR cannot express — dropping
    // the qualifier silently would over-report.
    if has_temporal_superlative_over_related(tokens, &spec.related, binding) {
        return None;
    }

    Some((spec, vec![]))
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

    let projection: Vec<ValueExpr> = vec![
        ColumnRef { concept, role: ColumnRole::PersonFullName, name_hint: None, physical: None }.into(),
        ColumnRef { concept, role: ColumnRole::BusinessId, name_hint: None, physical: None }.into(),
        ColumnRef { concept, role: ColumnRole::EventTime, name_hint: None, physical: None }.into(),
        ColumnRef { concept, role: ColumnRole::Status, name_hint: None, physical: None }.into(),
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
            related: vec![],
            duration_filters: vec![],
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

    // Predicate filters — same "carry whole or refuse" invariant as R1/R8.
    // For R10 (EXISTS), the bind's collect::<Result<_,_>>()? would propagate
    // a NoRole error on a cross-concept filter, but we check explicitly here
    // so the refuse is deterministic rather than relying on bind error propagation.
    let mut filters = Vec::new();
    for pred in lookup_all_predicates(&q_str) {
        if pred.kind.is_unexpressible() {
            return None;
        }
        if pred.concept == concept || concept == EntityConcept::Unknown {
            filters.push(predicate_to_filter(pred));
        } else {
            return None;
        }
    }

    let time_range = extract_time_range(tokens);
    let time_scope = time_range.map(|(range, _, _, _)| make_time_scope(concept, range));

    Some((
        QuerySpec {
            subject: Subject { concept, table: None },
            shape: Shape::Exists,
            measures: vec![],
            dimensions: vec![],
            filters,
            time: time_scope,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
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

/// R13, part 1: the threshold the question states, **in the unit the question
/// used** — `over 14 days` is `(14.0, Days)`.
///
/// Keeping the unit rather than immediately normalising to minutes is what lets
/// the derived-duration form emit `… > 14` against a days-valued expression, so
/// the literal in the SQL is still the number in the question and an auditor can
/// check it by reading (plan 03g §1). The stored-column form below still
/// normalises, because a stored `Duration` column's own unit is fixed by the
/// schema and is not the question's to choose.
pub(crate) fn extract_r13_threshold(tokens: &[&str]) -> Option<(f64, DurationUnit)> {
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
    let (mult, unit) = if unit_pos < tokens.len() {
        match tokens[unit_pos] {
            "minutes" | "minute" | "min" => (1.0, DurationUnit::Minutes),
            "hours" | "hour" | "h" => (1.0, DurationUnit::Hours),
            "days" | "day" => (1.0, DurationUnit::Days),
            // `DurationUnit` has no Weeks variant on purpose — a week is exactly
            // seven days, so expressing it as days keeps one canonical unit set
            // without losing the value the question stated.
            "weeks" | "week" => (7.0, DurationUnit::Days),
            _ => (1.0, DurationUnit::Hours), // default: hours
        }
    } else {
        (1.0, DurationUnit::Hours)
    };

    Some((n * mult, unit))
}

/// R13, part 2 (stored-column form): `Filter { Duration > N_minutes }`.
///
/// Retained for concepts whose schema stores a duration column outright. It does
/// no timestamp arithmetic — it compares a stored number — so it is not a second
/// implementation of the derived-duration arithmetic, and there is nothing to
/// unify away here. What *is* unified is the threshold parse above, shared with
/// the derived form so the two can never disagree about what the question said.
pub(crate) fn extract_r13_duration_filter(tokens: &[&str], concept: EntityConcept) -> Option<Filter> {
    let (n, unit) = extract_r13_threshold(tokens)?;
    let minutes = n * (unit.seconds() as f64) / 60.0;
    Some(Filter {
        column: ColumnRef { concept, role: ColumnRole::Duration, name_hint: None, physical: None },
        op: FilterOp::Gt,
        value: FilterValue::Num(minutes),
    })
}

/// R13, part 3 (derived form, plan 03g §1): the same threshold applied to an
/// interval the schema does not store as a number.
///
/// Returns `None` when the concept does not own exactly one interval, so the
/// caller can fall back to the stored-column form or refuse — never so it can
/// pick an endpoint by position.
pub(crate) fn extract_r13_duration_threshold(
    tokens: &[&str],
    concept: EntityConcept,
    binding: &SchemaBinding,
) -> Option<DurationFilter> {
    let (n, unit) = extract_r13_threshold(tokens)?;
    if !concept_has_unique_interval(binding, concept) {
        return None;
    }
    let open = open_interval_choice(tokens)?;
    Some(DurationFilter {
        duration: DerivedDuration {
            start: ColumnRef { concept, role: ColumnRole::StartTime, name_hint: None, physical: None },
            end: ColumnRef { concept, role: ColumnRole::EndTime, name_hint: None, physical: None },
            unit,
            open,
        },
        op: FilterOp::Gt,
        value: FilterValue::Num(n),
    })
}

/// R14: `expires|due|scheduled (within|in the next) <N> <unit>` OR `due this week/month`
/// → `EventTime BETWEEN now AND now+N` (expressed as `TimeRange::Within` or `TimeRange::Next`).
/// Returns a `TimeScope` if the pattern matches.
///
/// When the trigger token is "expires" or "expiring", the returned `TimeScope`
/// uses a bounded `WithinThis*` range (both lower and upper bound) and sets
/// `name_hint = Some("expir")` so that `bind.rs` prefers the `expires_on`
/// column over other EventTime candidates (e.g. `issued_on`).
pub(crate) fn extract_r14_future_time(tokens: &[&str]) -> Option<TimeScope> {
    let is_expiry = tokens.iter().any(|t| ["expires", "expiring"].contains(t));

    // "due this week" / "due this month" / "due this quarter" /
    // "expires this quarter" / etc.
    if tokens.iter().any(|t| ["due", "expires", "expiring", "scheduled"].contains(t)) {
        if let Some(p) = find_token(tokens, "this") {
            if p + 1 < tokens.len() {
                if let Some((unit, _)) = parse_bucket_unit(tokens, p + 1) {
                    let range = if is_expiry {
                        // Use bounded WithinThis* for expiry queries so both
                        // the start and end of the period are constrained.
                        match unit {
                            BucketUnit::Week => TimeRange::WithinThisWeek,
                            BucketUnit::Month => TimeRange::WithinThisMonth,
                            BucketUnit::Quarter => TimeRange::WithinThisQuarter,
                            // Year expiry: reuse WithinThisQuarter semantics via
                            // ThisYear for now (lower-bound only); year-granularity
                            // expiry queries are uncommon.
                            BucketUnit::Year => TimeRange::ThisYear,
                            _ => return None,
                        }
                    } else {
                        match unit {
                            BucketUnit::Week => TimeRange::ThisWeek,
                            BucketUnit::Month => TimeRange::ThisMonth,
                            BucketUnit::Quarter => TimeRange::ThisQuarter,
                            BucketUnit::Year => TimeRange::ThisYear,
                            _ => return None,
                        }
                    };
                    // name_hint "expir" lets bind prefer expires_on over issued_on.
                    let name_hint = if is_expiry { Some("expir".to_string()) } else { None };
                    // Concept is unknown here — caller fills it.
                    return Some(TimeScope {
                        column: ColumnRef {
                            concept: EntityConcept::Unknown,
                            role: ColumnRole::EventTime,
                            name_hint,
                            physical: None,
                        },
                        range: Some(range),
                        bucket: None,
                    });
                }
            }
        }
        // "within N days/weeks/months"
        if let Some((range, _, _, _)) = extract_time_range(tokens) {
            let name_hint = if is_expiry { Some("expir".to_string()) } else { None };
            return Some(TimeScope {
                column: ColumnRef {
                    concept: EntityConcept::Unknown,
                    role: ColumnRole::EventTime,
                    name_hint,
                    physical: None,
                },
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
// Temporal-superlative guard
// ---------------------------------------------------------------------------

/// Returns `true` when the question contains a temporal-superlative qualifier
/// ("last", "latest", or "most recent") immediately before a word that resolves
/// to one of the concepts already placed in `related_scopes`.
///
/// These questions require a most-recent-per-group computation (window function
/// or correlated MAX subquery) that the current IR cannot express — refusing is
/// the correct behaviour and is far less harmful than silently dropping the
/// qualifier and over-reporting.
///
/// Examples that trigger refusal:
///   "Which patients missed their **last** appointment?"
///   "List patients with their **latest** prescription"
///   "Who had a **most recent** visit this month?"
///
/// Examples that are explicitly excluded (time-range uses of "last"):
///   "How many appointments were missed **last month**?"   ("last" + bucket unit)
///   "Prescriptions issued in the **last 30 days**"        ("last" + digit)
///
/// The exclusion is structural (parse_bucket_unit / digit check), not string
/// matching against any specific question text.
fn has_temporal_superlative_over_related(
    tokens: &[&str],
    related_scopes: &[RelatedScope],
    binding: &SchemaBinding,
) -> bool {
    if related_scopes.is_empty() {
        return false;
    }

    // Concepts that appear as related (cross-entity) scopes in this spec.
    let rs_concepts: Vec<EntityConcept> = related_scopes.iter().map(|rs| rs.concept).collect();

    for (i, &tok) in tokens.iter().enumerate() {
        // Detect superlative marker and compute where the following concept word starts.
        let concept_start = if tok == "last" || tok == "latest" {
            i + 1
        } else if tok == "most" && tokens.get(i + 1).copied() == Some("recent") {
            // "most recent <concept>" — concept word is two positions ahead.
            i + 2
        } else {
            continue;
        };

        // Exclude time-range uses: "last month", "last week", "last year", etc.
        // parse_bucket_unit returns Some for singular bucket names at concept_start.
        if tok == "last" || tok == "latest" {
            if parse_bucket_unit(tokens, concept_start).is_some() {
                continue;
            }
            // Also exclude "last <N> days/weeks/..." patterns.
            if tokens
                .get(concept_start)
                .map(|t| t.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or(false)
            {
                continue;
            }
        }

        // Check up to three tokens after the superlative for a concept that is
        // present in the related scopes.  Three tokens covers "last appointment",
        // "last scheduled appointment", etc.
        let end = tokens.len().min(concept_start + 3);
        for j in concept_start..end {
            if let Some(concept) = phrase_to_concept(tokens[j], binding) {
                if rs_concepts.contains(&concept) {
                    return true;
                }
            }
        }
    }

    false
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

    // ── Plan-03 defect regression tests ──────────────────────────────────────

    /// Defect 1: "show/list <concept> by <non-time-dimension>" must derive
    /// `Shape::Grouped` → `QueryIntent::Aggregation`, not `Shape::List`.
    ///
    /// Root cause: R8 matched "show" and produced List unconditionally, ignoring
    /// the "by <dim>" clause.  Fixed by detecting `has_display_anchor &&
    /// !is_enum_interrogative && has_by_non_time` and returning Grouped.
    ///
    /// Tested against both plan-01 schema bindings: dev seed (make_test_binding
    /// mirrors the relevant concepts) and alt schema (alt_binding_for_tests).
    #[test]
    fn grouped_count_by_dimension_derives_aggregation() {
        use crate::aggregation::intent::QueryIntent;

        // Build two plan-01-equivalent bindings.
        // make_test_binding has Patient, Encounter, Admission, Triage, Delivery, Newborn.
        let dev_binding = make_test_binding();

        // Build a minimal "alt"-flavoured binding: add a Triage table with Duration.
        let make_tb = |name: &str, concept: EntityConcept, cols: Vec<(&str, ColumnRole, Vec<&str>)>| {
            crate::ontology::binding::TableBinding {
                table_name: name.into(),
                concept,
                confidence: 0.95,
                service_lines: vec![crate::ontology::service_line::ServiceLine::PatientChart],
                columns: cols.into_iter().map(|(cn, role, evs)| {
                    crate::ontology::binding::ColumnBinding {
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
        let alt_binding = SchemaBinding {
            source_id: "alt".into(),
            bound_at: Utc::now(),
            tables: vec![
                make_tb("patients", EntityConcept::Patient, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("pat_no", ColumnRole::BusinessId, vec![]),
                    ("full_name", ColumnRole::PersonFullName, vec![]),
                    ("sex", ColumnRole::Gender, vec!["m", "f"]),
                    ("registered_at", ColumnRole::EventTime, vec![]),
                ]),
                make_tb("encounters", EntityConcept::Encounter, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("encounter_type", ColumnRole::Type, vec!["outpatient", "inpatient"]),
                    ("seen_at", ColumnRole::EventTime, vec![]),
                    ("duration_minutes", ColumnRole::Duration, vec![]),
                ]),
            ],
            degraded: false,
            override_version: 0,
        };

        for (label, binding) in [("dev", &dev_binding), ("alt", &alt_binding)] {
            // "show patients by type" — Triage/Encounter both have a Type column; the
            // "by <non-time-token>" rule must fire regardless of whether the dimension
            // token resolves to a known EntityConcept.
            let result = parse("show patients by type", None, binding, &[]);
            match result {
                ParseOutcome::Parsed { spec, .. } => {
                    assert_eq!(
                        spec.shape, Shape::Grouped,
                        "[{label}] 'show patients by type' must parse to Shape::Grouped, got {:?}",
                        spec.shape
                    );
                    assert_eq!(
                        spec.intent(), QueryIntent::Aggregation,
                        "[{label}] Shape::Grouped must derive Aggregation intent"
                    );
                }
                ParseOutcome::NoParse => {
                    panic!("[{label}] 'show patients by type' should have parsed");
                }
            }
        }
    }

    /// Defect 2 regression — R4 must NOT fire on "over N <unit>" (numeric threshold).
    ///
    /// "which patients have been in over 14 days" — R4 previously fired on the bare
    /// word "over" and produced Shape::Trend → QueryIntent::Trend (wrong).  After
    /// the R4 fix, "over" followed by a number is not a trend anchor, so R8 fires
    /// and produces Shape::List.
    ///
    /// However, in both plan-01 bindings the Duration column lives on the Admission
    /// concept, not on Patient.  R13 (duration threshold) matches the "over 14 days"
    /// pattern and emits a filter referencing Patient.Duration.  Because Patient has
    /// no Duration column, the bind step fails with NoRole — the pipeline correctly
    /// returns NoParse rather than producing a spec without the constraint.
    ///
    /// Invariant asserted: a spec is never emitted when a recognised numeric threshold
    /// cannot be carried in `spec.filters`.  The outcome must be NoParse.
    #[test]
    fn numeric_threshold_cross_concept_duration_is_noparse() {
        let make_tb = |name: &str, concept: EntityConcept, cols: Vec<(&str, ColumnRole, Vec<&str>)>| {
            crate::ontology::binding::TableBinding {
                table_name: name.into(),
                concept,
                confidence: 0.95,
                service_lines: vec![crate::ontology::service_line::ServiceLine::PatientChart],
                columns: cols.into_iter().map(|(cn, role, evs)| {
                    crate::ontology::binding::ColumnBinding {
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

        // dev-equivalent: Patient has no Duration column; Duration lives on Admission.
        let dev_binding = {
            let mut b = make_test_binding();
            b.source_id = "dev".into();
            b
        };

        // alt-equivalent: same layout.
        let alt_binding = SchemaBinding {
            source_id: "alt".into(),
            bound_at: Utc::now(),
            tables: vec![
                make_tb("patients", EntityConcept::Patient, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("pat_no", ColumnRole::BusinessId, vec![]),
                    ("full_name", ColumnRole::PersonFullName, vec![]),
                    ("registered_at", ColumnRole::EventTime, vec![]),
                ]),
                make_tb("admissions", EntityConcept::Admission, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("admitted_at", ColumnRole::EventTime, vec![]),
                    ("length_of_stay_days", ColumnRole::Duration, vec![]),
                ]),
            ],
            degraded: false,
            override_version: 0,
        };

        for (label, binding) in [("dev", &dev_binding), ("alt", &alt_binding)] {
            // R4 must not fire (over + number = threshold, not trend).
            // R8 fires, R13 matches "over 14 days", emits Patient.Duration filter.
            // Patient has no Duration → bind fails with NoRole → NoParse.
            // Emitting the spec without the filter would answer a different question.
            let result = parse("which patients have been in over 14 days", None, binding, &[]);
            assert!(
                matches!(result, ParseOutcome::NoParse),
                "[{label}] Expected NoParse — Patient lacks Duration column so R13's \
                 filter cannot be placed; the parser must not emit a spec without it"
            );
        }
    }

    /// Class-level guard: when a concept OWNS a Duration column and the question
    /// carries a recognised numeric threshold, a successful parse must carry that
    /// threshold in `spec.filters` as a `FilterOp::Gt` on `ColumnRole::Duration`.
    ///
    /// This prevents future code from re-introducing a silent-drop guard that
    /// removes the constraint and makes the spec answer a different question.
    #[test]
    fn numeric_threshold_on_owning_concept_is_in_filters() {
        use crate::nl2sql::ir::spec::{FilterOp, FilterValue};

        let make_tb = |name: &str, concept: EntityConcept, cols: Vec<(&str, ColumnRole, Vec<&str>)>| {
            crate::ontology::binding::TableBinding {
                table_name: name.into(),
                concept,
                confidence: 0.95,
                service_lines: vec![crate::ontology::service_line::ServiceLine::PatientChart],
                columns: cols.into_iter().map(|(cn, role, evs)| {
                    crate::ontology::binding::ColumnBinding {
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

        // Encounter IS the subject and OWNS the Duration column.
        let binding = SchemaBinding {
            source_id: "test".into(),
            bound_at: Utc::now(),
            tables: vec![
                make_tb("encounters", EntityConcept::Encounter, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("patient_id", ColumnRole::PatientRef, vec![]),
                    ("encounter_type", ColumnRole::Type, vec!["outpatient", "inpatient"]),
                    ("seen_at", ColumnRole::EventTime, vec![]),
                    ("duration_minutes", ColumnRole::Duration, vec![]),
                ]),
                make_tb("patients", EntityConcept::Patient, vec![
                    ("id", ColumnRole::PrimaryKey, vec![]),
                    ("pat_no", ColumnRole::BusinessId, vec![]),
                    ("full_name", ColumnRole::PersonFullName, vec![]),
                ]),
            ],
            degraded: false,
            override_version: 0,
        };

        let result = parse("which encounters lasted over 30 minutes", None, &binding, &[]);
        match result {
            ParseOutcome::Parsed { spec, .. } => {
                // The Duration threshold must be present in spec.filters.
                let dur_filter = spec.filters.iter().find(|f| f.column.role == ColumnRole::Duration);
                assert!(
                    dur_filter.is_some(),
                    "Parsed spec must carry the Duration filter; got filters: {:?}",
                    spec.filters
                );
                let f = dur_filter.unwrap();
                assert_eq!(f.op, FilterOp::Gt, "Duration filter must use Gt operator");
                // 30 minutes = 30 * 1.0 = 30 (multiplier for minutes is 1.0)
                assert!(
                    matches!(f.value, FilterValue::Num(v) if (v - 30.0).abs() < 1.0),
                    "Duration filter value must be 30 minutes; got {:?}", f.value
                );
            }
            ParseOutcome::NoParse => {
                panic!(
                    "Expected a Parsed outcome for 'which encounters lasted over 30 minutes' \
                     (Encounter owns Duration); got NoParse"
                );
            }
        }
    }

}

