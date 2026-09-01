use crate::connectors::SourceKind;
use crate::error::AppResult;
use crate::foundry::FoundryManager;
use crate::foundry::router::ModelSpec;

use super::spec::{EmitSqlOutput, SqlExample, TableCard};

/// Build the system prompt for SQL generation, tuned per dialect.
pub fn system_prompt(dialect: SourceKind) -> String {
    let dialect_name = match dialect {
        SourceKind::Postgres => "PostgreSQL",
        SourceKind::Mysql => "MySQL",
        SourceKind::Mssql => "SQL Server (T-SQL)",
    };

    format!(
        "You are an expert {dialect_name} query writer for a health-records system.\n\
         Your job: write a single, valid {dialect_name} SELECT statement that answers \
         the user's question.\n\
         Rules:\n\
         - Only SELECT statements. No INSERT, UPDATE, DELETE, DDL, or multi-statement batches.\n\
         - Use only the tables and columns listed in the schema below.\n\
         - If the user requests a row count, use LIMIT/TOP for that count; otherwise omit it.\n\
         - Use table aliases when joining more than one table.\n\
         - Return exactly the SQL statement as plain text or in one ```sql code block.\n\
         - Do not return JSON, commentary, or an explanation."
    )
}

/// Approximate token count. Source projections and schema text are whitespace-
/// delimited, so word count is a conservative, model-independent proxy that
/// avoids loading a tokenizer on the request path (same convention as
/// `rag::apply_context_budget`).
fn approx_tokens(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Truncate `s` to at most `budget` whitespace-delimited tokens, appending a
/// marker so the model (and logs) can tell the schema was clipped.
fn clip_to_tokens(s: &str, budget: usize) -> String {
    if approx_tokens(s) <= budget {
        return s.to_string();
    }
    let clipped: Vec<&str> = s.split_whitespace().take(budget).collect();
    format!("{}\n[...schema truncated to fit model context...]", clipped.join(" "))
}

/// Format schema cards into the user message, dropping whole tables (least
/// relevant first, since `schema_cards` is ranked best-first by the linker)
/// until the formatted block fits `token_budget`. This is the primary defense
/// against the phi-4-mini context overrun (observed: 8,631 tokens requested
/// against a 4,224-token context) — a wide or many-table schema card set can
/// otherwise dwarf the model's window on its own, before examples or the
/// question are even added.
fn format_schema_budgeted(cards: &[TableCard], token_budget: usize) -> String {
    let mut kept = cards.len();
    loop {
        let block = format_schema(&cards[..kept]);
        if approx_tokens(&block) <= token_budget || kept == 0 {
            if kept == cards.len() {
                return block;
            }
            if kept == 0 {
                // Even a single table's card overflows the budget on its own;
                // clip its text rather than sending nothing (the model still
                // needs at least the primary table to have a chance at the SQL).
                let one_table = format_schema(&cards[..1.min(cards.len())]);
                return clip_to_tokens(&one_table, token_budget);
            }
            return block;
        }
        kept -= 1;
    }
}

/// Format schema cards into the user message.
fn format_schema(cards: &[TableCard]) -> String {
    if cards.is_empty() {
        return String::new();
    }
    let mut lines = vec!["## Schema\n".to_string()];
    for card in cards {
        lines.push(format!("### {}", card.table_name));
        lines.push(format!("Rows (approx): {}", card.row_count));
        let col_lines: Vec<String> = card
            .columns
            .iter()
            .map(|c| {
                let mut flags = vec![c.type_.clone()];
                if c.is_primary_key {
                    flags.push("PK".into());
                }
                if c.is_foreign_key {
                    flags.push("FK".into());
                }
                if !c.nullable {
                    flags.push("NOT NULL".into());
                }
                if !c.sample_values.is_empty() {
                    flags.push(format!(
                        "examples: {}",
                        c.sample_values[..c.sample_values.len().min(3)].join(", ")
                    ));
                }
                if let Some(distinct) = c.profile.approximate_distinct_count {
                    if distinct <= 100 {
                        flags.push(format!("approx distinct: {distinct}"));
                    }
                }
                if c.profile.sampled_rows > 0 {
                    if let Some(null_ratio) = c.profile.null_ratio {
                        if null_ratio > 0.0 {
                            flags.push(format!("approx nulls: {:.1}%", null_ratio * 100.0));
                        }
                    }
                }
                if let (Some(min), Some(max)) = (&c.profile.min, &c.profile.max) {
                    flags.push(format!("approx range: {min} to {max}"));
                }
                format!("  {} ({})", c.name, flags.join(", "))
            })
            .collect();
        lines.push(format!("Columns:\n{}", col_lines.join("\n")));
        if !card.fk_edges.is_empty() {
            let fk_lines: Vec<String> = card
                .fk_edges
                .iter()
                .map(|e| format!("  {} -> {}.{}", e.column, e.ref_table, e.ref_column))
                .collect();
            lines.push(format!("Foreign keys:\n{}", fk_lines.join("\n")));
        }
        let aliases: Vec<&str> = card
            .card_text
            .split(" Alias: ")
            .skip(1)
            .filter_map(|part| part.split(" Alias: ").next())
            .map(|alias| alias.trim().trim_end_matches('.'))
            .filter(|alias| !alias.is_empty())
            .collect();
        if !aliases.is_empty() {
            lines.push(format!("Business aliases: {}", aliases.join("; ")));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Format few-shot examples into the user message.
fn format_examples(examples: &[SqlExample]) -> String {
    if examples.is_empty() {
        return String::new();
    }
    let mut lines = vec!["## Examples\n".to_string()];
    for ex in examples {
        lines.push(format!("Q: {}", ex.question));
        lines.push(format!("SQL:\n```sql\n{}\n```\n", ex.sql));
    }
    lines.join("\n")
}

/// Assemble the user prompt within `token_budget`, reserving room for `tail`
/// (the question, and for a repair pass, the previous attempt + error). Sheds
/// budget pressure in order: few-shot examples first (they're a quality nicety,
/// not required for correctness), then whole schema tables (least-relevant
/// first, since callers pass cards ranked best-first), then a hard per-table
/// clip as a last resort. This is what stops the model being asked for 8,631
/// tokens against phi-4-mini's 4,224-token context — the schema+examples block
/// is now sized to what the model can actually read.
fn build_budgeted_user_prompt(
    schema_cards: &[TableCard],
    few_shots: &[SqlExample],
    tail: &str,
    token_budget: usize,
) -> String {
    let tail_tokens = approx_tokens(tail);
    let schema_budget = token_budget.saturating_sub(tail_tokens);

    // 1. Try the full schema plus all examples.
    let full_schema = format_schema(schema_cards);
    let examples_block = format_examples(few_shots);
    if approx_tokens(&full_schema) + approx_tokens(&examples_block) <= schema_budget {
        return format!("{full_schema}{examples_block}\n{tail}");
    }

    // 2. Drop examples; try the full schema alone.
    if approx_tokens(&full_schema) <= schema_budget {
        tracing::warn!(
            budget = schema_budget,
            "nl2sql prompt over token budget with examples; dropping few-shots"
        );
        return format!("{full_schema}\n{tail}");
    }

    // 3. Shed whole tables / clip as a last resort.
    tracing::warn!(
        budget = schema_budget,
        tables = schema_cards.len(),
        "nl2sql schema over token budget; trimming tables"
    );
    let schema_block = format_schema_budgeted(schema_cards, schema_budget);
    format!("{schema_block}\n{tail}")
}

/// Call the local SQL model to generate SQL for `question`. Plain or fenced SQL
/// is accepted; the caller must still run the mandatory AST validator.
pub async fn plan_sql(
    foundry: &FoundryManager,
    spec: &ModelSpec,
    dialect: SourceKind,
    schema_cards: &[TableCard],
    few_shots: &[SqlExample],
    question: &str,
    prompt_token_budget: usize,
) -> AppResult<EmitSqlOutput> {
    let system = system_prompt(dialect);
    let tail = format!("## Question\n{question}");
    let user = build_budgeted_user_prompt(schema_cards, few_shots, &tail, prompt_token_budget);

    foundry.plan_sql(spec, &system, &user).await
}

/// Repair pass: append the error from the first attempt and ask for a corrected query.
pub async fn plan_sql_repair(
    foundry: &FoundryManager,
    spec: &ModelSpec,
    dialect: SourceKind,
    schema_cards: &[TableCard],
    few_shots: &[SqlExample],
    question: &str,
    failed_sql: &str,
    error: &str,
    prompt_token_budget: usize,
) -> AppResult<EmitSqlOutput> {
    let system = system_prompt(dialect);
    let tail = format!(
        "## Question\n{question}\n\n\
         ## Previous attempt (failed)\n```sql\n{failed_sql}\n```\n\
         Error: {error}\n\n\
         Fix the query and call `emit_sql` with the corrected SQL."
    );
    let user = build_budgeted_user_prompt(schema_cards, few_shots, &tail, prompt_token_budget);

    foundry.plan_sql(spec, &system, &user).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl2sql::spec::{CardColumn, ColumnProfile};

    #[test]
    fn schema_prompt_includes_curated_aliases_and_useful_profiles() {
        let card = TableCard {
            source_id: "source-1".into(),
            table_name: "patients".into(),
            row_count: 1_000,
            columns: vec![CardColumn {
                name: "county_id".into(),
                type_: "integer".into(),
                nullable: true,
                is_primary_key: false,
                is_foreign_key: true,
                sample_values: vec![],
                profile: ColumnProfile {
                    approximate_distinct_count: Some(47),
                    null_ratio: Some(0.025),
                    min: Some("1".into()),
                    max: Some("47".into()),
                    sampled_rows: 200,
                },
            }],
            fk_edges: vec![],
            card_vector: None,
            card_text: "Table: patients Alias: home county means patients.county_id.".into(),
        };

        let prompt = format_schema(&[card]);

        assert!(prompt.contains("approx distinct: 47"));
        assert!(prompt.contains("approx nulls: 2.5%"));
        assert!(prompt.contains("approx range: 1 to 47"));
        assert!(prompt.contains("Business aliases: home county means patients.county_id"));
    }

    fn wide_card(table_name: &str, n_cols: usize) -> TableCard {
        TableCard {
            source_id: "source-1".into(),
            table_name: table_name.into(),
            row_count: 1_000,
            columns: (0..n_cols)
                .map(|i| CardColumn {
                    name: format!("column_{i}_with_a_reasonably_long_name"),
                    type_: "varchar(255)".into(),
                    nullable: true,
                    is_primary_key: false,
                    is_foreign_key: false,
                    sample_values: vec!["sample value one".into(), "sample value two".into()],
                    profile: ColumnProfile::default(),
                })
                .collect(),
            fk_edges: vec![],
            card_vector: None,
            card_text: format!("Table: {table_name}"),
        }
    }

    #[test]
    fn approx_tokens_counts_whitespace_delimited_words() {
        assert_eq!(approx_tokens("one two three"), 3);
        assert_eq!(approx_tokens(""), 0);
    }

    #[test]
    fn clip_to_tokens_truncates_and_marks_overflowing_text() {
        let text = "one two three four five";
        let clipped = clip_to_tokens(text, 2);
        assert_eq!(clipped, "one two\n[...schema truncated to fit model context...]");
        assert_eq!(clip_to_tokens(text, 100), text);
    }

    /// Regression: a live-SQL prompt (schema cards + few-shots + question) must
    /// never be assembled without regard to the model's context window. This
    /// reproduces the observed failure shape — several wide tables whose combined
    /// schema text alone would exceed phi-4-mini's 4,224-token context — and
    /// asserts the assembled prompt is capped to the configured budget.
    #[test]
    fn budgeted_prompt_stays_within_token_budget_for_wide_schemas() {
        let cards = vec![
            wide_card("patients", 40),
            wide_card("encounters", 40),
            wide_card("diagnoses", 40),
            wide_card("medications", 40),
        ];
        // Sanity check: the unbounded schema block alone would blow a small budget.
        assert!(approx_tokens(&format_schema(&cards)) > 500);

        let tail = "## Question\nhow many diagnoses per patient?";
        let budget = 500;
        let prompt = build_budgeted_user_prompt(&cards, &[], tail, budget);

        assert!(
            approx_tokens(&prompt) <= budget + approx_tokens(tail) + 20,
            "budgeted prompt ({} tokens) should stay near the {budget}-token budget",
            approx_tokens(&prompt)
        );
        // The question must never be the thing that gets dropped.
        assert!(prompt.contains("how many diagnoses per patient?"));
    }

    #[test]
    fn budgeted_prompt_drops_examples_before_shedding_tables() {
        let cards = vec![wide_card("patients", 5)];
        let few_shots = vec![SqlExample {
            question: "example question".into(),
            sql: "SELECT 1".into(),
            dialect: "postgres".into(),
            tables: vec![],
            question_vector: None,
        }];
        let tail = "## Question\nhow many patients?";

        // A generous budget keeps both schema and examples.
        let generous = build_budgeted_user_prompt(&cards, &few_shots, tail, 10_000);
        assert!(generous.contains("## Examples"));
        assert!(generous.contains("### patients"));

        // A budget that fits the schema but not the examples drops only the examples.
        let schema_tokens = approx_tokens(&format_schema(&cards));
        let tight = build_budgeted_user_prompt(&cards, &few_shots, tail, schema_tokens + 2);
        assert!(!tight.contains("## Examples"));
        assert!(tight.contains("### patients"));
    }

    #[test]
    fn format_schema_budgeted_sheds_least_relevant_tables_first() {
        // Cards are ranked best-first by the caller (the linker); the budgeted
        // formatter must keep the front of the slice and drop from the tail.
        let cards = vec![wide_card("patients", 20), wide_card("encounters", 20)];
        let full = format_schema(&cards);
        let one_table_tokens = approx_tokens(&format_schema(&cards[..1]));

        let trimmed = format_schema_budgeted(&cards, one_table_tokens + 5);
        assert!(trimmed.contains("### patients"));
        assert!(!trimmed.contains("### encounters"));
        assert!(approx_tokens(&trimmed) < approx_tokens(&full));
    }
}
