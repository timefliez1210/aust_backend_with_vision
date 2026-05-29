//! Episodic memory — timestamped event log with vector embeddings.
//!
//! Each episode is a one-line summary of a meaningful assistant event (tool
//! execution, decision, observation). The 768-dim embedding from
//! `embeddinggemma:300m` enables semantic similarity search at retrieval time,
//! with a recency boost applied in SQL.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;
use crate::llm::AssistantLlmProvider;

/// A single episode row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Episode {
    pub id: Uuid,
    pub summary: String,
    // Embedding is stored as a pgvector column; we fetch it as Option<Vec<f32>>
    // using a raw text cast in the SELECT.
    pub tags: Vec<String>,
    pub refs: Value,
    pub created_at: DateTime<Utc>,
    /// Similarity score (0–1) populated during retrieval; None for direct fetches.
    #[sqlx(default)]
    pub similarity: Option<f64>,
}

/// Append a new episode, generating and storing its embedding.
///
/// If the LLM embedding call fails the episode is still stored without an
/// embedding (NULL), so the event is not lost.
pub async fn append(
    pool: &PgPool,
    llm: &dyn AssistantLlmProvider,
    summary: &str,
    tags: &[&str],
    refs: Value,
) -> Result<Uuid> {
    let embedding: Option<Vec<f32>> = match llm.embed(summary).await {
        Ok(v) => {
            if v.len() == 768 {
                Some(v)
            } else {
                tracing::warn!(
                    "Unexpected embedding dimension: {} (expected 768)",
                    v.len()
                );
                None
            }
        }
        Err(e) => {
            tracing::warn!("Embedding failed for episode, storing without vector: {e}");
            None
        }
    };

    let id = Uuid::now_v7();
    let tags_arr: Vec<String> = tags.iter().map(|s| s.to_string()).collect();

    if let Some(emb) = embedding {
        // Store as a pgvector literal by casting the float array via SQL.
        let vec_literal = format!(
            "[{}]",
            emb.iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        sqlx::query(
            r#"
            INSERT INTO agent_episodes (id, summary, embedding, tags, refs)
            VALUES ($1, $2, $3::vector, $4, $5)
            "#,
        )
        .bind(id)
        .bind(summary)
        .bind(vec_literal)
        .bind(&tags_arr)
        .bind(&refs)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            r#"
            INSERT INTO agent_episodes (id, summary, embedding, tags, refs)
            VALUES ($1, $2, NULL, $3, $4)
            "#,
        )
        .bind(id)
        .bind(summary)
        .bind(&tags_arr)
        .bind(&refs)
        .execute(pool)
        .await?;
    }

    Ok(id)
}

/// Retrieve the `k` most relevant episodes for a query text.
///
/// Relevance is `(1 - cosine_distance) * recency_factor` where `recency_factor`
/// decays exponentially with age (half-life: 7 days).
///
/// Episodes without an embedding are excluded from similarity results.
pub async fn retrieve_similar(
    pool: &PgPool,
    llm: &dyn AssistantLlmProvider,
    query: &str,
    k: i64,
) -> Result<Vec<Episode>> {
    let embedding = llm.embed(query).await?;
    let vec_literal = format!(
        "[{}]",
        embedding
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );

    // Recency factor: exp(-age_in_days / 7).  Similarity range: 0–1.
    // Combined score: similarity * recency_factor.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(Uuid, String, Vec<String>, Value, DateTime<Utc>, f64)> = sqlx::query_as(
        r#"
        SELECT
            id,
            summary,
            tags,
            refs,
            created_at,
            (1.0 - (embedding <=> $1::vector))
                * EXP(-EXTRACT(EPOCH FROM (NOW() - created_at)) / 604800.0) AS score
        FROM agent_episodes
        WHERE embedding IS NOT NULL
        ORDER BY score DESC
        LIMIT $2
        "#,
    )
    .bind(vec_literal)
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

/// Fetch the most recent `k` episodes (fallback when no query embedding available).
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
