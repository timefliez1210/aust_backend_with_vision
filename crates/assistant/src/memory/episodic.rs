//! Episodic memory — a timestamped log of what Josie did and how Alex reacted.
//!
//! One row per meaningful event: a write a tool performed ("Alex: „trag Lange am
//! Freitag ein“ → schedule_inquiry …"), or how Alex resolved a confirmation.
//! Recall is hybrid, so it works with or without the local embedding model:
//!
//! * German full-text search over the summary (`search_tsv`, always on),
//! * meaning-based similarity on `embedding_local` (when `[memory] embeddings = "local"`),
//! * a boost for episodes about the entities the conversation is on (tags such
//!   as `inquiry:<uuid>`, the same strings as the session's `active_scopes`),
//!
//! all weighted by recency (half-life 30 days). The older 768-dim `embedding`
//! column was never populated — Ollama Cloud has no embeddings — and is unused.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::embedder;
use crate::error::Result;

/// A single episode row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Episode {
    pub id: Uuid,
    pub summary: String,
    pub tags: Vec<String>,
    pub refs: Value,
    pub created_at: DateTime<Utc>,
    /// Relevance score populated during retrieval; None for direct fetches.
    #[sqlx(default)]
    pub similarity: Option<f64>,
}

/// Longest summary stored for one episode.
const MAX_SUMMARY_CHARS: usize = 600;

/// Append an episode. The embedding is computed in-process when enabled; without
/// it the episode is still stored and found through full-text search.
pub async fn append(pool: &PgPool, summary: &str, tags: &[String], refs: Value) -> Result<Uuid> {
    let summary: String = summary.chars().take(MAX_SUMMARY_CHARS).collect();
    let vector = embedder::embed(&summary).await.map(|v| embedder::to_pgvector(&v));
    let id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO agent_episodes (id, summary, embedding_local, tags, refs)
        VALUES ($1, $2, $3::vector, $4, $5)
        "#,
    )
    .bind(id)
    .bind(&summary)
    .bind(vector)
    .bind(tags)
    .bind(&refs)
    .execute(pool)
    .await?;
    Ok(id)
}

/// The `k` most relevant episodes for `query`, favouring the given entity scopes.
///
/// An episode qualifies when it shares a search term with the query, is about
/// one of `scopes`, or — with embeddings on — is close in meaning (cosine ≥ 0.5).
/// Unrelated history is left out rather than padded in.
pub async fn retrieve(pool: &PgPool, query: &str, scopes: &[String], k: i64) -> Result<Vec<Episode>> {
    let vector = embedder::embed(query).await.map(|v| embedder::to_pgvector(&v));
    let terms = or_tsquery(query);
    let scope_tags: Vec<String> = scopes.iter().filter(|s| *s != "global").cloned().collect();

    #[allow(clippy::type_complexity)]
    let rows: Vec<(Uuid, String, Vec<String>, Value, DateTime<Utc>, f64)> = sqlx::query_as(
        r#"
        WITH scored AS (
            SELECT id, summary, tags, refs, created_at,
                   CASE WHEN $1::vector IS NOT NULL AND embedding_local IS NOT NULL
                        THEN 1.0 - (embedding_local <=> $1::vector) ELSE 0 END AS sem,
                   CASE WHEN $2 <> '' THEN ts_rank_cd(search_tsv, to_tsquery('german', $2), 32)
                        ELSE 0 END AS lex,
                   CASE WHEN tags && $3::text[] THEN 1 ELSE 0 END AS on_scope
            FROM agent_episodes
        )
        SELECT id, summary, tags, refs, created_at,
               (0.6 * sem + 0.4 * lex + 0.5 * on_scope)
                 * EXP(-EXTRACT(EPOCH FROM (NOW() - created_at)) / 2592000.0) AS score
        FROM scored
        WHERE sem >= 0.5 OR lex > 0 OR on_scope = 1
        ORDER BY score DESC
        LIMIT $4
        "#,
    )
    .bind(vector)
    .bind(&terms)
    .bind(&scope_tags)
    .bind(k)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, summary, tags, refs, created_at, score)| Episode {
            id,
            summary,
            tags,
            refs,
            created_at,
            similarity: Some(score),
        })
        .collect())
}

/// Fetch the most recent `k` episodes.
pub async fn fetch_recent(pool: &PgPool, k: i64) -> Result<Vec<Episode>> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(Uuid, String, Vec<String>, Value, DateTime<Utc>)> = sqlx::query_as(
        r#"
        SELECT id, summary, tags, refs, created_at
        FROM agent_episodes
        ORDER BY created_at DESC
        LIMIT $1
        "#,
    )
    .bind(k)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, summary, tags, refs, created_at)| Episode {
            id,
            summary,
            tags,
            refs,
            created_at,
            similarity: None,
        })
        .collect())
}

/// An OR-query of the message's words for `to_tsquery('german', …)`.
///
/// `plainto_tsquery` ANDs every word, so a whole sentence ("wann kommt die Crew
/// zu Frau Lange?") matched nothing. Words are reduced to letters/digits — which
/// also keeps tsquery syntax out — and short filler words are dropped.
pub(crate) fn or_tsquery(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let w = raw.to_lowercase();
        if w.chars().count() >= 3 && !words.contains(&w) {
            words.push(w);
        }
        if words.len() == 12 {
            break;
        }
    }
    words.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn or_tsquery_ors_content_words_and_strips_syntax() {
        assert_eq!(or_tsquery("Wann kommt die Crew zu Frau Lange?"), "wann | kommt | die | crew | frau | lange");
        assert_eq!(or_tsquery("a & b | !c"), "");
        assert_eq!(or_tsquery("Müller's Umzug: 17.10."), "müller | umzug");
    }

    /// Without the embedding model, recall still works on words and entities.
    #[tokio::test]
    async fn recall_finds_episodes_by_word_and_by_entity_without_embeddings() {
        let Ok(url) = std::env::var("DATABASE_URL") else { return };
        let Ok(pool) = sqlx::PgPool::connect(&url).await else { return };

        let marker = format!("Zuverlässigkeitstest{}", Uuid::now_v7().simple());
        let scope = format!("inquiry:{}", Uuid::now_v7());
        let a = append(
            &pool,
            &format!("Alex: „{marker} Klavier ins 3. OG“ → schedule_inquiry erledigt"),
            &["schedule_inquiry".to_string()],
            serde_json::json!({}),
        )
        .await
        .unwrap();
        let b = append(&pool, "Rechnung bezahlt markiert", &[scope.clone()], serde_json::json!({}))
            .await
            .unwrap();

        let by_word = retrieve(&pool, &format!("was war mit {marker}?"), &[], 5).await.unwrap();
        assert!(by_word.iter().any(|e| e.id == a), "found by a shared word");

        let by_scope = retrieve(&pool, "xyzzy", &[scope.clone()], 5).await.unwrap();
        assert!(by_scope.iter().any(|e| e.id == b), "found by the entity in scope");
        assert!(!by_scope.iter().any(|e| e.id == a), "unrelated episode left out");

        sqlx::query("DELETE FROM agent_episodes WHERE id = ANY($1)")
            .bind(vec![a, b])
            .execute(&pool)
            .await
            .ok();
    }
}
