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

/// Call the local SQL model to generate SQL for `question`. Plain or fenced SQL
/// is accepted; the caller must still run the mandatory AST validator.
pub async fn plan_sql(
    foundry: &FoundryManager,
    spec: &ModelSpec,
    dialect: SourceKind,
    schema_cards: &[TableCard],
    few_shots: &[SqlExample],
    question: &str,
) -> AppResult<EmitSqlOutput> {
    let system = system_prompt(dialect);
    let schema_block = format_schema(schema_cards);
    let examples_block = format_examples(few_shots);

    let user = format!("{schema_block}{examples_block}\n## Question\n{question}");

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
) -> AppResult<EmitSqlOutput> {
    let system = system_prompt(dialect);
    let schema_block = format_schema(schema_cards);
    let examples_block = format_examples(few_shots);

    let user = format!(
        "{schema_block}{examples_block}\n## Question\n{question}\n\n\
         ## Previous attempt (failed)\n```sql\n{failed_sql}\n```\n\
         Error: {error}\n\n\
         Fix the query and call `emit_sql` with the corrected SQL."
    );

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
}
