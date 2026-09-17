//! Text helpers shared by the NL-to-SQL pipeline and the IR shadow path.
//!
//! These are pure string functions: no DB access, no async, no Rocket types.

/// `true` if the question looks like it contains a first+last name (heuristic).
pub(crate) fn contains_likely_person_name(question: &str) -> bool {
    static PERSON: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?:\b(?:for|of|patient)\s+|\b)([A-Z][a-z]+(?:[-'][A-Z]?[a-z]+)?\s+[A-Z][a-z]+(?:[-'][A-Z]?[a-z]+)?)(?:'s\b|\b)",
        )
        .expect("valid person-name regex")
    });
    PERSON.is_match(question)
}

/// Extract a record-number-style identifier (e.g. "PT-00001", "SYN-2024-0001")
/// verbatim from raw text. Uppercased to match the seeded `patient_no` format;
/// the character class (alphanumerics and hyphens only) keeps the value safe to
/// inline in SQL.
///
/// The trailing group is optional and the digit run is `{4,8}`, not the former
/// `\d{2,4}-\d{2,6}` (mandatory second group): the actual seeded format is a
/// *single* hyphen with a 5-digit run (`patients.patient_no` is `PT-00001`,
/// `docker/dev-postgres/init/02_seed.sql:942`, on a `VARCHAR(20)` column,
/// `01_schema.sql:236`) — the old two-hyphen requirement never matched it, or
/// any other seeded business code, at all. The `{4,8}` floor is deliberate, not
/// just "wide enough for 5 digits": it's what keeps `"COVID-19"` (2 digits)
/// correctly unmatched (see the test below), which a plain `+` would not.
pub(crate) fn extract_record_identifier(text: &str) -> Option<String> {
    static ID: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\b[A-Za-z]{2,6}-\d{4,8}(?:-\d{2,6})?\b")
            .expect("valid identifier regex")
    });
    static UUID: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        )
        .expect("valid uuid regex")
    });
    // A raw UUID's hyphen-separated hex groups can themselves satisfy the ID
    // pattern above (e.g. `...-ebda-4216-8201-...` reads as letters-digits-digits),
    // handing back a meaningless fragment instead of the record it actually names.
    // Skip any ID match that falls entirely inside a UUID rather than return one.
    let uuid_span = UUID.find(text).map(|m| (m.start(), m.end()));
    ID.find_iter(text)
        .find(|m| uuid_span.is_none_or(|(s, e)| m.end() <= s || m.start() >= e))
        .map(|found| found.as_str().to_ascii_uppercase())
}

/// If an anaphoric follow-up ("the patient", "their …") lacks an identifier but a
/// prior turn contains one, return the question augmented with that identifier so
/// the deterministic compiler can ground it. Turns are oldest → newest; the most
/// recent identifier wins.
pub(crate) fn resolve_followup_question<'turn>(
    question: &str,
    prior_turns: impl Iterator<Item = &'turn str>,
) -> Option<String> {
    if extract_record_identifier(question).is_some() {
        return None;
    }
    let normalized = normalize_question(question);
    let anaphoric_phrase = ["the patient", "this patient", "that patient"]
        .iter()
        .any(|phrase| normalized.contains(phrase));
    let anaphoric_pronoun = normalized.split_whitespace().any(|token| {
        matches!(
            token,
            "their" | "his" | "her" | "they" | "them" | "she" | "he"
        )
    });
    if !anaphoric_phrase && !anaphoric_pronoun {
        return None;
    }
    let id = prior_turns.filter_map(extract_record_identifier).last()?;
    Some(format!("{} patient {id}", question.trim()))
}

/// Fold the question to lowercase alphanumeric tokens joined by single spaces —
/// used for keyword matching so punctuation and casing don't matter.
pub(crate) fn normalize_question(question: &str) -> String {
    question
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rows listed in full before the answer starts truncating. Someone asking
/// "which patients have asthma" wants the names; a ward-sized list still reads,
/// a hospital-sized one does not.
pub(crate) const MAX_LISTED_ROWS: usize = 25;

/// Render a live-source result as the answer text. Scalars read as a sentence;
/// anything else is listed row by row, because a bare "returned 7 rows" makes
/// the user ask a second question to see what the first one found.
pub(crate) fn summarize_result(columns: &[String], rows: &[Vec<serde_json::Value>]) -> String {
    if rows.is_empty() {
        return "No matching records were found.".to_string();
    }

    if columns.len() == 1 && rows.len() == 1 && rows[0].len() == 1 {
        let label = columns[0].replace('_', " ");
        return format!("{label}: {}.", cell_text(&rows[0][0]));
    }

    let listed = rows
        .iter()
        .take(MAX_LISTED_ROWS)
        .enumerate()
        .map(|(index, row)| format!("{}. {}", index + 1, render_row(columns, row)))
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    let noun = if rows.len() == 1 { "record" } else { "records" };
    let mut answer = format!(
        "{} matching {noun} from the live database:
{listed}",
        rows.len()
    );
    if rows.len() > MAX_LISTED_ROWS {
        answer.push_str(&format!(
            "
…and {} more.",
            rows.len() - MAX_LISTED_ROWS
        ));
    }
    answer
}

/// One result row as a line: the leading column carries the line (it is the name
/// or identifier in every template we compile), the rest qualify it.
pub(crate) fn render_row(columns: &[String], row: &[serde_json::Value]) -> String {
    let mut cells = row.iter().enumerate().filter(|(_, cell)| !cell.is_null());
    let Some((_, first)) = cells.next() else {
        return "(no value)".to_string();
    };
    let rest = cells
        .map(|(index, cell)| {
            let label = columns
                .get(index)
                .map(|column| column.replace('_', " "))
                .unwrap_or_default();
            if label.is_empty() {
                cell_text(cell)
            } else {
                format!("{label}: {}", cell_text(cell))
            }
        })
        .collect::<Vec<_>>();
    if rest.is_empty() {
        cell_text(first)
    } else {
        format!("{} ({})", cell_text(first), rest.join(", "))
    }
}

/// JSON cell as display text — strings unquoted, everything else as written.
pub(crate) fn cell_text(cell: &serde_json::Value) -> String {
    match cell {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Null => "—".to_string(),
        value => value.to_string(),
    }
}
