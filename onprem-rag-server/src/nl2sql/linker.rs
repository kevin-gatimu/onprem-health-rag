use mongodb::bson::{Bson, Document, doc};

use crate::config::Config;
use crate::documentdb::DocumentDb;
use crate::embed::embed_query;
use crate::error::AppResult;

use super::spec::{CardColumn, CardFkEdge, TableCard};

/// Find the most relevant tables for a question by cosine similarity over the
/// cardVector embeddings. Returns at most `top_k` cards, ranked best-first.
pub async fn link(
    db: &DocumentDb,
    config: &Config,
    question: &str,
    source_id: &str,
    top_k: usize,
) -> AppResult<Vec<TableCard>> {
    let query_vec = embed_query(config, question).await?;

    // Load all cards for this source. For a typical health-records DB this is
    // 10-50 tables; in-memory ranking is fast and avoids a cosmosSearch round-trip.
    use futures::TryStreamExt;
    let mut cursor = db
        .schema_catalog()
        .find(doc! { "source_id": source_id })
        .await?;

    let mut scored: Vec<(f32, TableCard)> = Vec::new();
    while let Some(raw) = cursor.try_next().await? {
        let vec = extract_vector(&raw);
        let score = if vec.len() == query_vec.len() {
            cosine_sim(&query_vec, &vec)
        } else {
            0.0
        };
        if let Ok(card) = doc_to_card(raw) {
            scored.push((score, card));
        }
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

    Ok(scored.into_iter().map(|(_, c)| c).collect())
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

fn extract_vector(doc: &Document) -> Vec<f32> {
    doc.get_array("cardVector")
        .map(|arr| {
            arr.iter()
                .filter_map(|b| match b {
                    Bson::Double(d) => Some(*d as f32),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn doc_to_card(doc: Document) -> Result<TableCard, mongodb::bson::de::Error> {
    let source_id = doc
        .get_str("source_id")
        .unwrap_or_default()
        .to_string();
    let table_name = doc
        .get_str("table_name")
        .unwrap_or_default()
        .to_string();
    let row_count = doc.get_i64("row_count").unwrap_or_default();
    let card_text = doc
        .get_str("card_text")
        .unwrap_or_default()
        .to_string();

    let columns = doc
        .get_array("columns")
        .ok()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.as_document())
                .map(|d| CardColumn {
                    name: d.get_str("name").unwrap_or_default().to_string(),
                    type_: d.get_str("type").unwrap_or_default().to_string(),
                    nullable: d.get_bool("nullable").unwrap_or(true),
                    is_primary_key: d.get_bool("is_primary_key").unwrap_or_default(),
                    is_foreign_key: d.get_bool("is_foreign_key").unwrap_or_default(),
                    sample_values: d
                        .get_array("sample_values")
                        .ok()
                        .map(|a| {
                            a.iter()
                                .filter_map(|b| b.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    let fk_edges = doc
        .get_array("fk_edges")
        .ok()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.as_document())
                .map(|d| CardFkEdge {
                    column: d.get_str("column").unwrap_or_default().to_string(),
                    ref_table: d.get_str("ref_table").unwrap_or_default().to_string(),
                    ref_column: d.get_str("ref_column").unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TableCard {
        source_id,
        table_name,
        row_count,
        columns,
        fk_edges,
        card_vector: None,
        card_text,
    })
}
