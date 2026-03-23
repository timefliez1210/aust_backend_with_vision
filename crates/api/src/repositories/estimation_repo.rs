//! Estimation repository — centralised queries for the `volume_estimations` table.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Row returned by insert_estimation.
#[derive(Debug, FromRow)]
pub(crate) struct EstimationRow {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub method: String,
    pub status: String,
    pub source_data: serde_json::Value,
    pub result_data: Option<serde_json::Value>,
    pub total_volume_m3: Option<f64>,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Insert a volume estimation record and return the full row.
///
/// **Caller**: `estimates::post_depth_sensor`, `estimates::post_vision`
/// **Why**: Creates a completed estimation with all data in one shot.
pub(crate) async fn insert(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    method: &str,
    source_data: &serde_json::Value,
    result_data: Option<&serde_json::Value>,
    total_volume_m3: f64,
    confidence_score: f64,
    now: DateTime<Utc>,
) -> Result<EstimationRow, sqlx::Error> {
    let row: EstimationRow = sqlx::query_as(
        r#"
        INSERT INTO volume_estimations
            (id, inquiry_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(method)
    .bind(source_data)
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(confidence_score)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Insert a volume estimation record without returning (fire-and-forget style).
///
/// **Caller**: `create_inquiry`, `trigger_estimate`, `handle_complete_inquiry`
/// **Why**: Creates estimation for manual/inventory methods where the caller doesn't need
///          the returned row.
pub(crate) async fn insert_no_return(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    method: &str,
    source_data: &serde_json::Value,
    result_data: Option<&serde_json::Value>,
    total_volume_m3: f64,
    confidence_score: f64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO volume_estimations
            (id, inquiry_id, method, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(method)
    .bind(source_data)
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(confidence_score)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Pre-create a processing estimation row (status='processing') for polling.
///
/// **Caller**: `trigger_estimate_upload`, `trigger_video_upload`, `handle_submission`
/// **Why**: The frontend polls for estimation status; the row must exist before the
///          background task starts.
pub(crate) async fn create_processing(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    method: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        &format!(
            "INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, created_at) \
             VALUES ($1, $2, '{}', 'processing', '{{}}', NOW())",
            method
        ),
    )
    .bind(id)
    .bind(inquiry_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update source_data on an estimation (e.g. with S3 keys after upload).
///
/// **Caller**: `trigger_estimate_upload`, `handle_submission`
/// **Why**: S3 keys are written after upload so the admin UI can show images/videos.
pub(crate) async fn update_source_data(
    pool: &PgPool,
    estimation_id: Uuid,
    source_data: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE volume_estimations SET source_data = $1 WHERE id = $2")
        .bind(source_data)
        .bind(estimation_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark an estimation as failed.
///
/// **Caller**: Background task error handlers
/// **Why**: Estimation failures need to be recorded for the frontend to show error state.
pub(crate) async fn mark_failed(pool: &PgPool, estimation_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE volume_estimations SET status = 'failed' WHERE id = $1 AND status = 'processing'",
    )
    .bind(estimation_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the latest estimation ID, method, and source_data for an inquiry.
///
/// **Caller**: `update_inquiry_items`
/// **Why**: Item editing needs the latest estimation to update.
pub(crate) async fn fetch_latest_for_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<(Uuid, String, Option<serde_json::Value>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, method, source_data FROM volume_estimations WHERE inquiry_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}

/// Upsert a completed estimation (INSERT or UPDATE on conflict by ID).
///
/// **Caller**: `process_submission_background`, `process_video_background`
/// **Why**: The estimation row may have been pre-created with status='processing' by the caller.
///          This upserts to 'completed' with all result data.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    method: &str,
    source_data: &serde_json::Value,
    result_data: Option<&serde_json::Value>,
    total_volume_m3: f64,
    confidence_score: f64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO volume_estimations
            (id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, 'completed', $4, $5, $6, $7, $8)
        ON CONFLICT (id) DO UPDATE SET
            method            = EXCLUDED.method,
            status            = 'completed',
            source_data       = EXCLUDED.source_data,
            result_data       = EXCLUDED.result_data,
            total_volume_m3   = EXCLUDED.total_volume_m3,
            confidence_score  = EXCLUDED.confidence_score
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(method)
    .bind(source_data)
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(confidence_score)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update result_data and total_volume_m3 on an estimation.
///
/// **Caller**: `update_inquiry_items`
/// **Why**: Admin edits items, which changes both the result_data JSON and the total volume.
pub(crate) async fn update_results(
    pool: &PgPool,
    estimation_id: Uuid,
    result_data: &serde_json::Value,
    total_volume_m3: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE volume_estimations SET result_data = $1, total_volume_m3 = $2 WHERE id = $3",
    )
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(estimation_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a processing estimation and return the full row.
///
/// **Caller**: `estimates::video_estimate`
/// **Why**: Video estimations need the returned row to build the VolumeEstimation response.
pub(crate) async fn insert_processing_returning(
    pool: &PgPool,
    id: Uuid,
    inquiry_id: Uuid,
    method: &str,
    source_data: &serde_json::Value,
    now: DateTime<Utc>,
) -> Result<EstimationRow, sqlx::Error> {
    sqlx::query_as(
        r#"
        INSERT INTO volume_estimations (id, inquiry_id, method, status, source_data, total_volume_m3, confidence_score, created_at)
        VALUES ($1, $2, $3, 'processing', $4, NULL, NULL, $5)
        RETURNING id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        "#,
    )
    .bind(id)
    .bind(inquiry_id)
    .bind(method)
    .bind(source_data)
    .bind(now)
    .fetch_one(pool)
    .await
}

/// Fetch an estimation by ID.
///
/// **Caller**: `estimates::get_estimate`, `estimates::delete_estimate`
/// **Why**: Returns the full estimation row for display or pre-deletion inspection.
pub(crate) async fn fetch_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<EstimationRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, inquiry_id, method, status, source_data, result_data, total_volume_m3, confidence_score, created_at
        FROM volume_estimations WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Delete an estimation by ID.
///
/// **Caller**: `estimates::delete_estimate`
/// **Why**: Removes the DB row after S3 cleanup.
pub(crate) async fn delete_by_id(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM volume_estimations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark an estimation as completed with results.
///
/// **Caller**: `estimates::process_video_background`
/// **Why**: Updates a processing estimation to completed with result data.
pub(crate) async fn mark_completed(
    pool: &PgPool,
    estimation_id: Uuid,
    result_data: Option<&serde_json::Value>,
    total_volume_m3: f64,
    confidence_score: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE volume_estimations
        SET status = 'completed', result_data = $1, total_volume_m3 = $2, confidence_score = $3
        WHERE id = $4
        "#,
    )
    .bind(result_data)
    .bind(total_volume_m3)
    .bind(confidence_score)
    .bind(estimation_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Count estimations still processing for an inquiry.
///
/// **Caller**: `estimates::process_video_background`
/// **Why**: Determines if all video estimations have finished before triggering offer generation.
pub(crate) async fn count_processing(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM volume_estimations WHERE inquiry_id = $1 AND status = 'processing'",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Sum completed estimation volumes for an inquiry.
///
/// **Caller**: `estimates::process_video_background`, `estimates::delete_estimate`
/// **Why**: Calculates combined volume from all completed estimations.
pub(crate) async fn sum_completed_volume(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<f64, sqlx::Error> {
    let (total,): (Option<f64>,) = sqlx::query_as(
        "SELECT SUM(total_volume_m3) FROM volume_estimations WHERE inquiry_id = $1 AND status = 'completed'",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await?;
    Ok(total.unwrap_or(0.0))
}

/// Update inquiry volume and status.
///
/// **Caller**: `estimates::delete_estimate`, `estimates::process_video_background`
/// **Why**: Keeps the inquiry's estimated_volume_m3 in sync with estimation results.
pub(crate) async fn update_inquiry_volume_status(
    pool: &PgPool,
    inquiry_id: Uuid,
    volume: f64,
    status: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inquiries SET estimated_volume_m3 = $1, status = $2, updated_at = $3 WHERE id = $4")
        .bind(volume)
        .bind(status)
        .bind(now)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update inquiry status to processing.
///
/// **Caller**: `estimates::video_estimate`
/// **Why**: Sets status while video estimation is underway.
pub(crate) async fn update_inquiry_processing(
    pool: &PgPool,
    inquiry_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inquiries SET status = 'processing', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Estimation row with full detail for the inquiry builder.
#[derive(Debug, FromRow)]
pub(crate) struct EstimationDetailRow {
    pub id: Uuid,
    pub method: String,
    pub status: String,
    pub total_volume_m3: Option<f64>,
    #[sqlx(default)]
    pub confidence_score: Option<f64>,
    pub result_data: Option<serde_json::Value>,
    pub source_data: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// Fetch the latest completed estimation for an inquiry (full detail projection).
///
/// **Caller**: `inquiry_builder::build_inquiry_response`
/// **Why**: Inquiry detail needs estimation metadata, result items, and source images.
pub(crate) async fn fetch_completed_for_inquiry(
    pool: &PgPool,
    inquiry_id: Uuid,
) -> Result<Option<EstimationDetailRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, method, status, total_volume_m3, confidence_score,
               result_data, source_data, created_at
        FROM volume_estimations
        WHERE inquiry_id = $1 AND status = 'completed'
        ORDER BY created_at DESC LIMIT 1
        "#,
    )
    .bind(inquiry_id)
    .fetch_optional(pool)
    .await
}
