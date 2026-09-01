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
         - Do not include a LIMIT or TOP clause; the system adds one.\n\
         - Use table aliases when joining more than one table.\n\
         - Return the query by calling the `emit_sql` tool. Do not write SQL in prose."
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

/// Call phi-4-mini to generate SQL for `question`. Returns the raw `EmitSqlOutput`
/// (sql field may still violate safety rules; caller runs validate_sql next).
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

    let user = format!(
        "{schema_block}{examples_block}\n## Question\n{question}"
    );

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
