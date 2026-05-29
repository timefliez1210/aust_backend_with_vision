//! Admin endpoints for inspecting the assistant's action audit log (`agent_actions`).
//!
//! All routes require the admin JWT — they are mounted under `/api/v1/admin/agent-activity`
//! by [`router`].
//!
//! # Endpoints
//! - `GET  /admin/agent-activity`        — paginated list with filters
//! - `GET  /admin/agent-activity/stats`  — aggregated counts since a given time
//! - `GET  /admin/agent-activity/:id`    — full row including raw args + result JSONB

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;

use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(list_activity))
        .route("/stats", get(activity_stats))
        .route("/{id}", get(get_activity))
}

// ── List ─────────────────────────────────────────────────────────────────────

/// Query parameters for `GET /admin/agent-activity`.
#[derive(Debug, Deserialize)]
pub struct ListActivityQuery {
    pub tool_name: Option<String>,
    pub session_id: Option<Uuid>,
    pub since: Option<DateTime<Utc>>,
    pub only_errors: Option<bool>,
    pub only_confirmed: Option<bool>,
    /// Maximum rows to return (default 100, max 500).
    pub limit: Option<i64>,
    /// Cursor: the `id` of the last item on the previous page (UUID v7, so sortable).
    pub cursor: Option<Uuid>,
}

/// One item in the list response — summaries only (full payloads via detail endpoint).
#[derive(Debug, Serialize, FromRow)]
pub struct ActivityListItem {
    pub id: Uuid,
    pub session_id: Uuid,
    pub tool_name: String,
    /// First 200 chars of args JSON.
    pub args_summary: String,
    /// First 200 chars of result JSON, or the error message.
    pub result_summary: Option<String>,
    pub duration_ms: Option<i32>,
    pub confirmed: bool,
    pub ts: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ListActivityResponse {
    pub items: Vec<ActivityListItem>,
    pub next_cursor: Option<Uuid>,
}

// Type alias to keep the complex tuple readable in query_as.
type ActionRow = (
    Uuid,
    Uuid,
    String,
    Value,
    Option<Value>,
    Option<String>,
    Option<i32>,
    Option<Uuid>,
    DateTime<Utc>,
);

type DetailRow = (
    Uuid,
    Uuid,
    String,
    Value,
    Option<Value>,
    Option<String>,
    Option<i32>,
    Option<Uuid>,
    DateTime<Utc>,
);

/// `GET /api/v1/admin/agent-activity` — Paginated audit log of assistant tool calls.
///
/// **Caller**: Admin dashboard "Assistent" tab.
/// **Filters**: `tool_name`, `session_id`, `since` (RFC 3339), `only_errors`, `only_confirmed`.
/// **Pagination**: cursor-based via `cursor=<uuid>` + `limit` (default 100, max 500).
///
/// # Returns
/// `200 OK` with `{ items: [...], next_cursor? }`.
pub async fn list_activity(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(q): Query<ListActivityQuery>,
) -> Result<Json<ListActivityResponse>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    // Fetch one extra row so we know whether there is a next page.
    let fetch_limit = limit + 1;

    // We build the query with runtime conditionals via a WHERE-clause string.
    // sqlx query_as! requires a static query string, so we use the dynamic API.
    let rows: Vec<ActionRow> = sqlx::query_as(
        r#"
        SELECT id, session_id, tool_name, args, result, error_message,
               duration_ms, confirmed_action_id, created_at
        FROM agent_actions
        WHERE ($1::text    IS NULL OR tool_name       = $1)
          AND ($2::uuid    IS NULL OR session_id      = $2)
          AND ($3::timestamptz IS NULL OR created_at  >= $3)
          AND ($4::bool    IS NULL OR $4 = false OR error_message IS NOT NULL)
          AND ($5::bool    IS NULL OR $5 = false OR confirmed_action_id IS NOT NULL)
          AND ($6::uuid    IS NULL OR id < $6)
        ORDER BY id DESC
        LIMIT $7
        "#,
    )
    .bind(q.tool_name.as_deref())
    .bind(q.session_id)
    .bind(q.since)
    .bind(q.only_errors)
    .bind(q.only_confirmed)
    .bind(q.cursor)
    .bind(fetch_limit)
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::from)?;

    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();

    let next_cursor = if has_more {
        rows.last().map(|r| r.0)
    } else {
        None
    };

    let items = rows
        .into_iter()
        .map(
            |(id, session_id, tool_name, args, result, error_message, duration_ms, confirmed_action_id, created_at)| {
                let args_summary = truncate_json(&args, 200);
                let result_summary = if let Some(msg) = error_message {
                    Some(msg.chars().take(200).collect())
                } else {
                    result.as_ref().map(|v| truncate_json(v, 200))
                };
                ActivityListItem {
                    id,
                    session_id,
                    tool_name,
                    args_summary,
                    result_summary,
                    duration_ms,
                    confirmed: confirmed_action_id.is_some(),
                    ts: created_at,
                }
            },
        )
        .collect();

    Ok(Json(ListActivityResponse { items, next_cursor }))
}

// ── Detail ────────────────────────────────────────────────────────────────────

/// Full `agent_actions` row including raw args and result JSONB.
#[derive(Debug, Serialize)]
pub struct ActivityDetail {
    pub id: Uuid,
    pub session_id: Uuid,
    pub tool_name: String,
    pub args: Value,
    pub result: Option<Value>,
    pub error_message: Option<String>,
    pub duration_ms: Option<i32>,
    pub confirmed_action_id: Option<Uuid>,
    pub ts: DateTime<Utc>,
}

/// `GET /api/v1/admin/agent-activity/:id` — Full action row including raw JSONB payloads.
///
/// **Caller**: Admin detail panel when the user clicks a list item.
///
/// # Returns
/// `200 OK` with `ActivityDetail` JSON, or `404` if not found.
pub async fn get_activity(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<ActivityDetail>, ApiError> {
    let row: Option<DetailRow> = sqlx::query_as(
            r#"
            SELECT id, session_id, tool_name, args, result, error_message,
                   duration_ms, confirmed_action_id, created_at
            FROM agent_actions
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::from)?;

    let (id, session_id, tool_name, args, result, error_message, duration_ms, confirmed_action_id, created_at) =
        row.ok_or_else(|| ApiError::NotFound(format!("Agent-Aktion {id} nicht gefunden")))?;

    Ok(Json(ActivityDetail {
        id,
        session_id,
        tool_name,
        args,
        result,
        error_message,
        duration_ms,
        confirmed_action_id,
        ts: created_at,
    }))
}

// ── Stats ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    pub since: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct ToolStats {
    pub tool_name: String,
    pub count: i64,
    pub error_count: i64,
    pub confirmed_count: i64,
}

#[derive(Debug, Serialize)]
pub struct ActivityStats {
    pub total_calls: i64,
    pub error_count: i64,
    pub confirmed_count: i64,
    /// Error rate 0.0–1.0.
    pub error_rate: f64,
    pub by_tool: Vec<ToolStats>,
}

/// `GET /api/v1/admin/agent-activity/stats?since=<rfc3339>` — Aggregated activity metrics.
///
/// **Caller**: Admin dashboard assistant stats card.
///
/// # Query Parameters
/// - `since` — optional RFC 3339 timestamp; defaults to last 7 days.
///
/// # Returns
/// `200 OK` with `ActivityStats` JSON.
pub async fn activity_stats(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<ActivityStats>, ApiError> {
    let since = q.since.unwrap_or_else(|| Utc::now() - chrono::Duration::days(7));

    let (total_calls, error_count, confirmed_count): (i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT
            COUNT(*),
            COUNT(*) FILTER (WHERE error_message IS NOT NULL),
            COUNT(*) FILTER (WHERE confirmed_action_id IS NOT NULL)
        FROM agent_actions
        WHERE created_at >= $1
        "#,
    )
    .bind(since)
    .fetch_one(&state.db)
    .await
    .map_err(ApiError::from)?;

    let by_tool_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        r#"
        SELECT
            tool_name,
            COUNT(*),
            COUNT(*) FILTER (WHERE error_message IS NOT NULL),
            COUNT(*) FILTER (WHERE confirmed_action_id IS NOT NULL)
        FROM agent_actions
        WHERE created_at >= $1
        GROUP BY tool_name
        ORDER BY COUNT(*) DESC
        "#,
    )
    .bind(since)
    .fetch_all(&state.db)
    .await
    .map_err(ApiError::from)?;

    let by_tool = by_tool_rows
        .into_iter()
        .map(|(tool_name, count, ec, cc)| ToolStats {
            tool_name,
            count,
            error_count: ec,
            confirmed_count: cc,
        })
        .collect();

    let error_rate = if total_calls == 0 {
        0.0
    } else {
        error_count as f64 / total_calls as f64
    };

    Ok(Json(ActivityStats {
        total_calls,
        error_count,
        confirmed_count,
        error_rate,
        by_tool,
    }))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Serialize `v` to JSON string, then truncate to `max_chars` characters.
fn truncate_json(v: &Value, max_chars: usize) -> String {
    let s = v.to_string();
    if s.len() <= max_chars {
        s
    } else {
        let mut truncated: String = s.chars().take(max_chars).collect();
        truncated.push('…');
        truncated
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use chrono::Duration;
    use hyper::Request;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::test_helpers::{generate_test_jwt, test_app_state_with_pool};

    async fn build_test_app(pool: sqlx::PgPool) -> axum::Router {
        let state = test_app_state_with_pool(pool).await;
        crate::create_router(state)
    }

    // Helper: insert an agent_session and return its id.
    async fn insert_session(pool: &sqlx::PgPool) -> Uuid {
        let id = Uuid::now_v7();
        let chat_id: i64 = -(id.as_u128() as i64).abs() - 1;
        let now = Utc::now();
        sqlx::query(
            r#"
            INSERT INTO agent_sessions (id, chat_id, turns, turn_count, created_at, updated_at)
            VALUES ($1, $2, '[]'::jsonb, 0, $3, $4)
            "#,
        )
        .bind(id)
        .bind(chat_id)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .expect("insert session");
        id
    }

    async fn insert_action(
        pool: &sqlx::PgPool,
        session_id: Uuid,
        tool_name: &str,
        error_message: Option<&str>,
        confirmed_action_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            r#"
            INSERT INTO agent_actions
                (id, session_id, tool_name, args, result, error_message, confirmed_action_id, created_at)
            VALUES ($1, $2, $3, '{"q":1}'::jsonb, NULL, $4, $5, $6)
            "#,
        )
        .bind(id)
        .bind(session_id)
        .bind(tool_name)
        .bind(error_message)
        .bind(confirmed_action_id)
        .bind(created_at)
        .execute(pool)
        .await
        .expect("insert action");
        id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_list_returns_items(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        insert_action(&pool, session_id, "get_inquiry", None, None, now).await;

        let response = app
            .oneshot(
                Request::get("/api/v1/admin/agent-activity")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["items"].as_array().unwrap().len() >= 1);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_list_filter_only_errors(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        insert_action(&pool, session_id, "send_email", Some("SMTP error"), None, now).await;
        insert_action(&pool, session_id, "get_inquiry", None, None, now).await;

        let response = app
            .oneshot(
                Request::get("/api/v1/admin/agent-activity?only_errors=true")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = v["items"].as_array().unwrap();
        // All returned items should have an error (result_summary non-null).
        assert!(!items.is_empty());
        assert!(items.iter().all(|i| !i["result_summary"].is_null()));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_list_filter_tool_name(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        insert_action(&pool, session_id, "get_inquiry", None, None, now).await;
        insert_action(&pool, session_id, "send_email", None, None, now).await;

        let response = app
            .oneshot(
                Request::get("/api/v1/admin/agent-activity?tool_name=send_email")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = v["items"].as_array().unwrap();
        assert!(items.iter().all(|i| i["tool_name"] == json!("send_email")));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_list_filter_since(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        // Old row: should NOT appear in result.
        insert_action(&pool, session_id, "old_tool", None, None, now - Duration::days(5)).await;
        // Recent row: should appear.
        insert_action(&pool, session_id, "new_tool", None, None, now).await;

        // Encode the since timestamp as a simple ISO string (no special chars needed here).
        let since = (now - Duration::hours(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .replace('+', "%2B");
        let uri = format!("/api/v1/admin/agent-activity?since={since}");

        let response = app
            .oneshot(
                Request::get(uri)
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = v["items"].as_array().unwrap();
        // Only new_tool should be present.
        assert!(items.iter().all(|i| i["tool_name"] == json!("new_tool")));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_get_activity_detail(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        let action_id = insert_action(&pool, session_id, "get_inquiry", None, None, now).await;

        let response = app
            .oneshot(
                Request::get(format!("/api/v1/admin/agent-activity/{action_id}"))
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], json!(action_id.to_string()));
        assert!(v["args"].is_object(), "args should be full JSONB");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_get_activity_detail_not_found(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let missing = Uuid::now_v7();
        let response = app
            .oneshot(
                Request::get(format!("/api/v1/admin/agent-activity/{missing}"))
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_stats_endpoint(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        insert_action(&pool, session_id, "get_inquiry", None, None, now).await;
        insert_action(&pool, session_id, "send_email", Some("err"), None, now).await;

        let response = app
            .oneshot(
                Request::get("/api/v1/admin/agent-activity/stats")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["total_calls"].as_i64().unwrap() >= 2);
        assert!(v["error_count"].as_i64().unwrap() >= 1);
        assert!(v["by_tool"].is_array());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_list_pagination_cursor(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        for i in 0..5i32 {
            insert_action(
                &pool,
                session_id,
                "tool",
                None,
                None,
                now - Duration::milliseconds(i as i64 * 10),
            )
            .await;
        }

        // First page: limit=2
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/admin/agent-activity?limit=2")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let page1: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(page1["items"].as_array().unwrap().len(), 2);
        let cursor = page1["next_cursor"].as_str().expect("next_cursor present");

        // Second page via cursor.
        let response2 = app
            .oneshot(
                Request::get(format!("/api/v1/admin/agent-activity?limit=2&cursor={cursor}"))
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX)
            .await
            .unwrap();
        let page2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert!(!page2["items"].as_array().unwrap().is_empty());
    }

    /// S3 regression: `only_errors=false` must return all rows (including non-error ones),
    /// not zero rows. The old filter `($4 = true AND error_message IS NOT NULL)` made
    /// `false` exclude everything.
    #[sqlx::test(migrations = "../../migrations")]
    async fn test_only_errors_false_returns_all_rows(pool: sqlx::PgPool) {
        let token = generate_test_jwt();
        let app = build_test_app(pool.clone()).await;
        let now = Utc::now();
        let session_id = insert_session(&pool).await;
        // One error row, one success row.
        insert_action(&pool, session_id, "failing_tool", Some("boom"), None, now).await;
        insert_action(&pool, session_id, "ok_tool", None, None, now).await;

        // only_errors=false → must see both rows (at least 2 from this test).
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/admin/agent-activity?only_errors=false")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let items = v["items"].as_array().unwrap();
        assert!(
            items.len() >= 2,
            "only_errors=false should return all rows including non-error ones, got {} items",
            items.len()
        );

        // only_errors=true → must see only error rows.
        let response2 = app
            .oneshot(
                Request::get("/api/v1/admin/agent-activity?only_errors=true")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response2.status(), 200);
        let body2 = axum::body::to_bytes(response2.into_body(), usize::MAX).await.unwrap();
        let v2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        let items2 = v2["items"].as_array().unwrap();
        assert!(
            items2.iter().all(|i| !i["result_summary"].is_null()),
            "only_errors=true should return only rows with errors"
        );
    }
}
