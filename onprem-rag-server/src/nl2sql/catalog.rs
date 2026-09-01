use mongodb::bson::{Bson, Document, doc};
use crate::config::Config;
use crate::connectors::{SourceSpec, connector};
use crate::documentdb::{DocumentDb, SCHEMA_CATALOG};
use crate::embed::embed_documents;
use crate::error::AppResult;

use super::spec::{CardColumn, CardFkEdge, TableCard};

/// Build or refresh schema cards for every table in a source. Overwrites existing
/// cards for this source_id. New tables get a new card; deleted tables keep their
/// stale card until the next refresh.
pub async fn refresh_catalog(
    db: &DocumentDb,
    config: &Config,
    spec: &SourceSpec,
    source_id: &str,
) -> AppResult<usize> {
    let conn = connector(spec);
    let tables = conn.get_schema().await?;

    let sample_limit = config.router.nl2sql_sample_values;
    let mut cards: Vec<TableCard> = Vec::with_capacity(tables.len());

    for table in &tables {
        let sample_values = if sample_limit > 0 {
            match conn.fetch_table(&table.name, &[], Some(sample_limit as i64)).await {
                Ok(rows) => extract_samples(&rows, 3),
                Err(_) => std::collections::HashMap::new(),
            }
        } else {
            std::collections::HashMap::new()
        };

        let columns: Vec<CardColumn> = table
            .columns
            .iter()
            .map(|c| CardColumn {
                name: c.name.clone(),
                type_: c.type_.clone(),
                nullable: c.nullable,
                is_primary_key: c.is_primary_key,
                is_foreign_key: c.is_foreign_key,
                sample_values: sample_values.get(&c.name).cloned().unwrap_or_default(),
            })
            .collect();

        let fk_edges: Vec<CardFkEdge> = table
            .fk_edges
            .iter()
            .map(|e| CardFkEdge {
                column: e.column.clone(),
                ref_table: e.ref_table.clone(),
                ref_column: e.ref_column.clone(),
            })
            .collect();

        let card_text = build_card_text(&table.name, table.row_count, &columns, &fk_edges);

        cards.push(TableCard {
            source_id: source_id.to_string(),
            table_name: table.name.clone(),
            row_count: table.row_count,
            columns,
            fk_edges,
            card_vector: None,
            card_text,
        });
    }

    // Embed all card texts in one batch.
    let texts: Vec<String> = cards.iter().map(|c| c.card_text.clone()).collect();
    let vectors = embed_documents(config, texts).await?;

    let count = cards.len();
    for (card, vec) in cards.iter_mut().zip(vectors.into_iter()) {
        card.card_vector = Some(vec);
    }

    // Upsert each card. The filter is the logical key {source_id, table_name}.
    let coll = db.schema_catalog();
    for card in &cards {
        let mut doc = card_to_bson(card)?;
        // Remove _id so MongoDB manages it.
        doc.remove("_id");

        coll.update_one(
            doc! { "source_id": &card.source_id, "table_name": &card.table_name },
            doc! { "$set": doc },
        )
        .upsert(true)
        .await?;
    }

    tracing::info!(source_id, count, "schema catalog refreshed");
    Ok(count)
}

/// Create the cosmosSearch vector index and the $text index on `schema_catalog`.
pub async fn ensure_nl2sql_indexes(db: &DocumentDb, dims: usize) -> AppResult<()> {
    db.db
        .run_command(doc! {
            "createIndexes": SCHEMA_CATALOG,
            "indexes": [{
                "name": "schema_catalog_cardVector_cosmos",
                "key": { "cardVector": "cosmosSearch" },
                "cosmosSearchOptions": {
                    "kind": "vector-ivf",
                    "numLists": 100,
                    "similarity": "COS",
                    "dimensions": dims as i32,
                }
            }]
        })
        .await?;

    db.db
        .run_command(doc! {
            "createIndexes": SCHEMA_CATALOG,
            "indexes": [{
                "name": "schema_catalog_text",
                "key": { "card_text": "text" }
            }]
        })
        .await?;

    tracing::info!(dims, "ensured nl2sql schema_catalog indexes");
    Ok(())
}

/// Render a card to a BSON Document. The card_vector (Vec<f32>) serializes as
/// Array([Double, ...]) which round-trips correctly through BSON.
fn card_to_bson(card: &TableCard) -> AppResult<Document> {
    let vector_bson: Vec<Bson> = card
        .card_vector
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|&f| Bson::Double(f as f64))
        .collect();

    let cols_bson: Vec<Bson> = card
        .columns
        .iter()
        .map(|c| {
            Bson::Document(doc! {
                "name": &c.name,
                "type": &c.type_,
                "nullable": c.nullable,
                "is_primary_key": c.is_primary_key,
                "is_foreign_key": c.is_foreign_key,
                "sample_values": c.sample_values.iter().map(|s| Bson::String(s.clone())).collect::<Vec<_>>(),
            })
        })
        .collect();

    let fk_bson: Vec<Bson> = card
        .fk_edges
        .iter()
        .map(|e| {
            Bson::Document(doc! {
                "column": &e.column,
                "ref_table": &e.ref_table,
                "ref_column": &e.ref_column,
            })
        })
        .collect();

    Ok(doc! {
        "source_id": &card.source_id,
        "table_name": &card.table_name,
        "row_count": card.row_count,
        "columns": cols_bson,
        "fk_edges": fk_bson,
        "cardVector": vector_bson,
        "card_text": &card.card_text,
    })
}

/// Build the human-readable card text embedded into the vector store.
fn build_card_text(
    table: &str,
    row_count: i64,
    columns: &[CardColumn],
    fk_edges: &[CardFkEdge],
) -> String {
    let mut lines = vec![format!("Table: {table} ({row_count} rows)")];

    let col_parts: Vec<String> = columns
        .iter()
        .map(|c| {
            let mut parts = vec![format!("{} {}", c.name, c.type_)];
            if c.is_primary_key {
                parts.push("PK".into());
            }
            if c.is_foreign_key {
                parts.push("FK".into());
            }
            if !c.nullable {
                parts.push("NOT NULL".into());
            }
            if !c.sample_values.is_empty() {
                parts.push(format!("e.g. {}", c.sample_values[..c.sample_values.len().min(3)].join(", ")));
            }
            parts.join(" ")
        })
        .collect();
    lines.push(format!("Columns: {}", col_parts.join(", ")));

    if !fk_edges.is_empty() {
        let fk_parts: Vec<String> = fk_edges
            .iter()
            .map(|e| format!("{} -> {}.{}", e.column, e.ref_table, e.ref_column))
            .collect();
        lines.push(format!("FK: {}", fk_parts.join(", ")));
    }

    lines.join("\n")
}

/// Pull up to `n` distinct sample values per column from the fetched rows.
fn extract_samples(
    rows: &[crate::connectors::FetchedRow],
    n: usize,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for row in rows {
        for (col, val) in &row.fields {
            let entry = out.entry(col.clone()).or_default();
            if entry.len() >= n {
                continue;
            }
            if let Some(s) = crate::connectors::value_to_plain(val) {
                if !entry.contains(&s) {
                    entry.push(s);
                }
            }
        }
    }
    out
}
