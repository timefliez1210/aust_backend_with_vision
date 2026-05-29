//! Recording offer adjustment observations to the DB.
//!
//! Every time the assistant proposes an offer and Alex edits it, we record the
//! difference in `offer_observations`. Over time this dataset trains the
//! `LinfaPredictor` in Phase 5.

use chrono::DateTime;
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::Result;
use crate::learning::features::OfferFeatures;

/// A row from the `offer_observations` table.
#[derive(Debug, sqlx::FromRow)]
pub struct ObservationRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub offer_id: Uuid,
    pub features: Value,
    pub proposed: Value,
    pub final_offer: Option<Value>,
    pub edit_distance: Option<Value>,
    pub used_in_training: bool,
    pub created_at: DateTime<Utc>,
}

/// Record an offer observation (proposed vs. final) in the database.
///
/// `edit_distance` is a structured diff (e.g. `{"price_delta_cents": -5000}`).
/// If the offer was accepted without edits, `final_offer` and `edit_distance`
/// are `None`.
pub async fn record_observation(
    pool: &PgPool,
    inquiry_id: Uuid,
    offer_id: Uuid,
    features: &OfferFeatures,
    proposed: &Value,
    final_offer: Option<&Value>,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let features_json = serde_json::to_value(features)?;

    // Compute a simple edit_distance if both proposed and final are present.
    let edit_distance = final_offer.map(|fin| compute_edit_distance(proposed, fin));

    sqlx::query(
        r#"
        INSERT INTO offer_observations
            (id, inquiry_id, offer_id, features, proposed, final, edit_distance)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(offer_id)
    .bind(&features_json)
    .bind(proposed)
    .bind(final_offer)
    .bind(edit_distance.as_ref())
    .execute(pool)
    .await?;

    Ok(id)
}

/// Fetch all observations not yet used in a training run.
pub async fn fetch_unused(pool: &PgPool) -> Result<Vec<ObservationRow>> {
    let rows = sqlx::query_as(
        r#"
        SELECT id, inquiry_id, offer_id, features, proposed,
               final AS final_offer, edit_distance, used_in_training, created_at
        FROM offer_observations
        WHERE NOT used_in_training
        ORDER BY created_at ASC
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Mark a list of observation IDs as used in training.
pub async fn mark_used(pool: &PgPool, ids: &[Uuid]) -> Result<()> {
    sqlx::query("UPDATE offer_observations SET used_in_training = TRUE WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await?;
    Ok(())
}

/// Compute a simple structured diff between two JSON offer objects.
///
/// Currently extracts `price_cents` delta. Phase 5 will extend this.
fn compute_edit_distance(proposed: &Value, final_offer: &Value) -> Value {
    let proposed_price = proposed["price_cents"].as_i64().unwrap_or(0);
    let final_price = final_offer["price_cents"].as_i64().unwrap_or(0);
    let delta = final_price - proposed_price;
    serde_json::json!({ "price_delta_cents": delta })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compute_edit_distance_calculates_delta() {
        let proposed = json!({"price_cents": 100000});
        let final_offer = json!({"price_cents": 95000});
        let dist = compute_edit_distance(&proposed, &final_offer);
        assert_eq!(dist["price_delta_cents"], -5000);
    }

    #[test]
    fn compute_edit_distance_zero_when_equal() {
        let p = json!({"price_cents": 80000});
        let f = json!({"price_cents": 80000});
        let dist = compute_edit_distance(&p, &f);
        assert_eq!(dist["price_delta_cents"], 0);
    }
}
