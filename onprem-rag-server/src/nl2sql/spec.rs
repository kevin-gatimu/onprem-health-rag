//! Types shared by the nl2sql pipeline stages.

use serde::{Deserialize, Serialize};

/// One column inside a schema card, as stored in `schema_catalog`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnProfile {
    /// Sample-based cardinality estimate, capped by the table's estimated row count.
    pub approximate_distinct_count: Option<i64>,
    pub null_ratio: Option<f64>,
    /// Aggregate bounds are captured only for numeric and date/time columns.
    pub min: Option<String>,
    pub max: Option<String>,
    pub sampled_rows: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_foreign_key: bool,
    /// Representative values sampled at catalog-refresh time (empty when sampling is off).
    #[serde(default)]
    pub sample_values: Vec<String>,
    /// Bounded aggregate profile. It never contains unrestricted row-level values.
    #[serde(default)]
    pub profile: ColumnProfile,
}

/// A FK relationship stored on the table card (mirrors `connectors::FkEdge`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardFkEdge {
    pub column: String,
    pub ref_table: String,
    pub ref_column: String,
}

/// One schema-card document from `schema_catalog`, returned by the linker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableCard {
    pub source_id: String,
    pub table_name: String,
    pub row_count: i64,
    pub columns: Vec<CardColumn>,
    #[serde(default)]
    pub fk_edges: Vec<CardFkEdge>,
    /// BGE-M3 embedding of `card_text`, stored as `cardVector` in DocumentDB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_vector: Option<Vec<f32>>,
    /// Human-readable card text (embedded + full-text indexed).
    pub card_text: String,
}

/// A validated few-shot (question → SQL) pair from `sql_examples`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlExample {
    pub question: String,
    pub sql: String,
    pub dialect: String,
    /// Tables referenced by the SQL (used for partial-match scoring).
    #[serde(default)]
    pub tables: Vec<String>,
    /// BGE-M3 embedding of `question`, stored as `questionVector` in DocumentDB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_vector: Option<Vec<f32>>,
}

/// Internal representation of a generated SQL statement.
///
/// The local model returns plain or fenced SQL; the Foundry adapter isolates it
/// into this type before the mandatory AST validation step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmitSqlOutput {
    /// The generated SELECT statement.
    pub sql: String,
}
