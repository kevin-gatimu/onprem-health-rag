use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};

use mongodb::bson::{Bson, Document, doc};

use crate::config::Config;
use crate::documentdb::DocumentDb;
use crate::embed::embed_query;
use crate::error::AppResult;

use super::spec::{CardColumn, CardFkEdge, ColumnProfile, TableCard};

#[derive(Clone)]
struct CachedGraph {
    version: String,
    cards: Vec<(Vec<f32>, TableCard)>,
}

static GRAPH_CACHE: OnceLock<RwLock<HashMap<String, CachedGraph>>> = OnceLock::new();

fn graph_cache() -> &'static RwLock<HashMap<String, CachedGraph>> {
    GRAPH_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn invalidate_source(source_id: &str) {
    if let Ok(mut cache) = graph_cache().write() {
        cache.remove(source_id);
    }
}

/// Retain only cards whose table name is in `scope` (plan 05 §3, hook 1).
///
/// Applied **before** scoring so the explicit-mention boost in
/// `prioritize_table_mentions` cannot lift an out-of-scope table back into the
/// candidate set. `None` or an empty scope means no narrowing.
///
/// The resulting card list is what `prepare_auto_query_deterministic` turns into
/// the `allowed_tables` argument of the existing `validate::validate_sql` guard,
/// so narrowing here narrows that one guard rather than introducing a second.
fn apply_scope(scored: &mut Vec<(f32, TableCard)>, scope: Option<&[String]>) {
    let Some(scope) = scope else { return };
    if scope.is_empty() {
        return;
    }
    scored.retain(|(_, card)| {
        scope
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&card.table_name))
    });
}

/// Find the most relevant tables for a question by cosine similarity over the
/// cardVector embeddings. Returns at most `top_k` cards, ranked best-first.
///
/// `scope` is an agent's read allow-list of physical table names (see
/// `agents::kind::AgentKind::scope`); `None` means no narrowing. It is a
/// relevance boundary, not an authorisation one.
pub async fn link(
    db: &DocumentDb,
    config: &Config,
    question: &str,
    source_id: &str,
    top_k: usize,
    scope: Option<&[String]>,
) -> AppResult<Vec<TableCard>> {
    let query_vec = embed_query(config, question).await?;
    let mut scored = rank_cached_cards(db, &query_vec, &[source_id.to_string()]).await?;
    apply_scope(&mut scored, scope);
    prioritize_table_mentions(&mut scored, question);
    Ok(select_with_relationships(scored, top_k))
}

/// Select the source whose best schema card is most relevant, then return only
/// cards from that source. A chat query never federates across databases.
pub async fn link_best_source(
    db: &DocumentDb,
    config: &Config,
    question: &str,
    source_ids: &[String],
    top_k: usize,
    scope: Option<&[String]>,
) -> AppResult<Option<(String, Vec<TableCard>)>> {
    if source_ids.is_empty() {
        return Ok(None);
    }
    let query_vec = embed_query(config, question).await?;
    let mut scored = rank_cached_cards(db, &query_vec, source_ids).await?;
    apply_scope(&mut scored, scope);
    prioritize_table_mentions(&mut scored, question);
    let Some(source_id) = scored.first().map(|(_, card)| card.source_id.clone()) else {
        return Ok(None);
    };
    let source_cards = scored
        .into_iter()
        .filter(|(_, card)| card.source_id == source_id)
        .collect();
    Ok(Some((
        source_id,
        select_with_relationships(source_cards, top_k),
    )))
}

async fn active_versions(
    db: &DocumentDb,
    source_ids: &[String],
) -> AppResult<HashMap<String, String>> {
    use futures::TryStreamExt;

    let mut cursor = db
        .schema_catalog_state()
        .find(doc! { "_id": { "$in": source_ids } })
        .await?;
    let mut active = HashMap::new();
    while let Some(state) = cursor.try_next().await? {
        if let (Ok(source_id), Ok(version)) =
            (state.get_str("_id"), state.get_str("active_version"))
        {
            active.insert(source_id.to_string(), version.to_string());
        }
    }
    Ok(active)
}

async fn rank_cached_cards(
    db: &DocumentDb,
    query_vec: &[f32],
    source_ids: &[String],
) -> AppResult<Vec<(f32, TableCard)>> {
    let versions = active_versions(db, source_ids).await?;
    let mut all_cards = Vec::new();
    for source_id in source_ids {
        let Some(version) = versions.get(source_id) else {
            continue;
        };
        let cached = graph_cache()
            .read()
            .ok()
            .and_then(|cache| cache.get(source_id).cloned())
            .filter(|entry| &entry.version == version);
        let graph = match cached {
            Some(entry) => entry,
            None => load_graph(db, source_id, version).await?,
        };
        all_cards.extend(graph.cards);
    }

    let mut scored = all_cards
        .into_iter()
        .map(|(vector, card)| {
            let score = if vector.len() == query_vec.len() {
                cosine_sim(query_vec, &vector)
            } else {
                0.0
            };
            (score, card)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored)
}

async fn load_graph(db: &DocumentDb, source_id: &str, version: &str) -> AppResult<CachedGraph> {
    use futures::TryStreamExt;

    let overrides = db
        .schema_metadata_overrides()
        .find_one(doc! { "_id": source_id })
        .await?;
    let aliases = overrides
        .as_ref()
        .and_then(|value| value.get_array("aliases").ok())
        .cloned()
        .unwrap_or_default();
    let relationships = overrides
        .as_ref()
        .and_then(|value| value.get_array("relationships").ok())
        .cloned()
        .unwrap_or_default();

    let mut cursor = db
        .schema_catalog()
        .find(doc! { "source_id": source_id, "catalog_version": version })
        .await?;
    let mut cards = Vec::new();
    while let Some(raw) = cursor.try_next().await? {
        let vector = extract_vector(&raw);
        if let Ok(mut card) = doc_to_card(raw) {
            for alias in aliases.iter().filter_map(Bson::as_document) {
                if alias.get_str("table").ok() == Some(card.table_name.as_str()) {
                    if let Ok(value) = alias.get_str("alias") {
                        match alias.get_str("column") {
                            Ok(column) => card.card_text.push_str(&format!(
                                " Alias: {value} means {}.{column}.",
                                card.table_name
                            )),
                            Err(_) => card
                                .card_text
                                .push_str(&format!(" Alias: {value} means {}.", card.table_name)),
                        }
                    }
                }
            }
            for edge in relationships.iter().filter_map(Bson::as_document) {
                if edge.get_str("from_table").ok() == Some(card.table_name.as_str()) {
                    if let (Ok(column), Ok(ref_table), Ok(ref_column)) = (
                        edge.get_str("from_column"),
                        edge.get_str("to_table"),
                        edge.get_str("to_column"),
                    ) {
                        card.fk_edges.push(CardFkEdge {
                            column: column.to_string(),
                            ref_table: ref_table.to_string(),
                            ref_column: ref_column.to_string(),
                        });
                    }
                }
            }
            cards.push((vector, card));
        }
    }
    let graph = CachedGraph {
        version: version.to_string(),
        cards,
    };
    if let Ok(mut cache) = graph_cache().write() {
        cache.insert(source_id.to_string(), graph.clone());
    }
    Ok(graph)
}

fn select_with_relationships(scored: Vec<(f32, TableCard)>, top_k: usize) -> Vec<TableCard> {
    if top_k == 0 {
        return Vec::new();
    }
    let seed_names: HashSet<String> = scored
        .iter()
        .take(top_k)
        .map(|(_, card)| card.table_name.to_ascii_lowercase())
        .collect();
    let mut required = seed_names.clone();

    // Include only each seed's one-hop FK targets so required lookup joins validate.
    for (_, card) in scored.iter().take(top_k) {
        let card_name = card.table_name.to_ascii_lowercase();
        for edge in &card.fk_edges {
            let ref_name = edge.ref_table.to_ascii_lowercase();
            if seed_names.contains(&card_name) {
                required.insert(ref_name);
            }
        }
    }

    scored
        .into_iter()
        .filter_map(|(_, card)| {
            required
                .remove(&card.table_name.to_ascii_lowercase())
                .then_some(card)
        })
        .collect()
}

fn prioritize_table_mentions(scored: &mut [(f32, TableCard)], question: &str) {
    scored.sort_by(|a, b| {
        let a_mentioned = card_mentioned(question, &a.1);
        let b_mentioned = card_mentioned(question, &b.1);
        b_mentioned
            .cmp(&a_mentioned)
            .then_with(|| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
    });
}

fn card_mentioned(question: &str, card: &TableCard) -> bool {
    if table_name_mentioned(question, &card.table_name) {
        return true;
    }
    let question = question.to_ascii_lowercase();
    card.card_text
        .split("Alias: ")
        .skip(1)
        .filter_map(|part| part.split(" means ").next())
        .any(|alias| question.contains(&alias.to_ascii_lowercase()))
}

fn table_name_mentioned(question: &str, table_name: &str) -> bool {
    let question_tokens: Vec<String> = question
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect();
    let table = table_name
        .rsplit('.')
        .next()
        .unwrap_or(table_name)
        .to_ascii_lowercase();
    let table_tokens: Vec<&str> = table
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();

    !table_tokens.is_empty()
        && table_tokens.iter().all(|table_token| {
            question_tokens.iter().any(|question_token| {
                question_token == table_token
                    || question_token.strip_suffix('s') == Some(*table_token)
                    || table_token.strip_suffix('s') == Some(question_token.as_str())
            })
        })
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

pub(crate) fn doc_to_card(doc: Document) -> Result<TableCard, mongodb::bson::de::Error> {
    let source_id = doc.get_str("source_id").unwrap_or_default().to_string();
    let table_name = doc.get_str("table_name").unwrap_or_default().to_string();
    let row_count = doc.get_i64("row_count").unwrap_or_default();
    let card_text = doc.get_str("card_text").unwrap_or_default().to_string();

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
                    profile: d
                        .get_document("profile")
                        .ok()
                        .map(|profile| ColumnProfile {
                            approximate_distinct_count: profile
                                .get_i64("approximate_distinct_count")
                                .ok(),
                            null_ratio: profile.get_f64("null_ratio").ok(),
                            min: profile.get_str("min").ok().map(str::to_string),
                            max: profile.get_str("max").ok().map(str::to_string),
                            sampled_rows: profile.get_i64("sampled_rows").unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::{prioritize_table_mentions, select_with_relationships, table_name_mentioned};
    use crate::nl2sql::spec::{CardFkEdge, TableCard};

    fn card(table_name: &str) -> TableCard {
        TableCard {
            source_id: "source-1".into(),
            table_name: table_name.into(),
            row_count: 0,
            columns: Vec::new(),
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: String::new(),
        }
    }

    #[test]
    fn recognizes_singular_and_plural_table_mentions() {
        assert!(table_name_mentioned("List 5 patients", "patients"));
        assert!(table_name_mentioned("Count each patient", "patients"));
        assert!(!table_name_mentioned("List providers", "patients"));
    }

    #[test]
    fn exact_table_mentions_outrank_embedding_scores() {
        let mut scored = vec![(0.9, card("patient_allergies")), (0.2, card("patients"))];
        prioritize_table_mentions(&mut scored, "List 5 patients");
        assert_eq!(scored[0].1.table_name, "patients");
    }

    #[test]
    fn seed_own_foreign_key_target_is_included() {
        let mut patients = card("patients");
        patients.fk_edges.push(CardFkEdge {
            column: "county_id".into(),
            ref_table: "counties".into(),
            ref_column: "id".into(),
        });
        let selected = select_with_relationships(
            vec![
                (0.9, patients),
                (0.1, card("facilities")),
                (0.05, card("counties")),
            ],
            1,
        );
        let names: Vec<&str> = selected
            .iter()
            .map(|card| card.table_name.as_str())
            .collect();
        assert_eq!(names, vec!["patients", "counties"]);
    }

    #[test]
    fn seeded_hub_does_not_include_tables_that_reference_it() {
        let patients = card("patients");
        let mut encounters = card("encounters");
        encounters.fk_edges.push(CardFkEdge {
            column: "patient_id".into(),
            ref_table: "patients".into(),
            ref_column: "id".into(),
        });
        let selected = select_with_relationships(
            vec![(0.9, patients), (0.2, encounters), (0.1, card("counties"))],
            1,
        );
        let names: Vec<&str> = selected
            .iter()
            .map(|card| card.table_name.as_str())
            .collect();
        assert_eq!(names, vec!["patients"]);
    }
}
