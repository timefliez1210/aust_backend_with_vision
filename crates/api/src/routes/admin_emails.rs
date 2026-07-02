use axum::{
    extract::{Path, Query, State},
    http::header,
    response::Response,
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use std::sync::Arc;
use uuid::Uuid;

use aust_core::models::TokenClaims;
use crate::repositories::{admin_repo, offer_repo};
use crate::routes::admin::mime_from_ext;
use crate::{ApiError, AppState};

// --- Email Threads ---

#[derive(Debug, Deserialize)]
pub(super) struct ListEmailThreadsQuery {
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct EmailThreadListItem {
    id: Uuid,
    customer_id: Uuid,
    customer_email: Option<String>,
    customer_name: Option<String>,
    inquiry_id: Option<Uuid>,
    subject: Option<String>,
    message_count: i64,
    last_message_at: Option<DateTime<Utc>>,
    last_direction: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(super) struct EmailThreadListResponse {
    threads: Vec<EmailThreadListItem>,
    total: i64,
}

/// `GET /api/v1/admin/emails` — List email threads with customer info and last-message metadata.
///
/// **Caller**: Axum router / admin dashboard "E-Mails" tab.
/// **Why**: Provides an inbox-style view of all email threads: customer name/email,
/// message count, last message direction, and timestamp. Supports full-text search on
/// customer name, email, and thread subject.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `query` — optional `search`, `limit`, `offset`
///
/// # Returns
/// `200 OK` with `EmailThreadListResponse` containing `threads` and `total`.
pub(super) async fn list_email_threads(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Query(query): Query<ListEmailThreadsQuery>,
) -> Result<Json<EmailThreadListResponse>, ApiError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .search
        .map(|s| format!("%{s}%"))
        .unwrap_or_else(|| "%".to_string());

    let repo_threads = admin_repo::list_email_threads(&state.db, &search, limit, offset).await?;
    let threads: Vec<EmailThreadListItem> = repo_threads
        .into_iter()
        .map(|t| EmailThreadListItem {
            id: t.id, customer_id: t.customer_id, customer_email: t.customer_email,
            customer_name: t.customer_name, inquiry_id: t.inquiry_id, subject: t.subject,
            message_count: t.message_count, last_message_at: t.last_message_at,
            last_direction: t.last_direction, created_at: t.created_at,
        })
        .collect();

    let total = admin_repo::count_email_threads(&state.db, &search).await?;

    Ok(Json(EmailThreadListResponse { threads, total }))
}

#[derive(Debug, Serialize)]
pub(super) struct EmailThreadDetailResponse {
    thread: EmailThreadDetail,
    messages: Vec<EmailMessageItem>,
}

#[derive(Debug, Serialize)]
pub(super) struct EmailThreadDetail {
    id: Uuid,
    customer_id: Uuid,
    customer_email: Option<String>,
    customer_name: Option<String>,
    inquiry_id: Option<Uuid>,
    subject: Option<String>,
    /// Filename of the active offer's PDF, if the thread's inquiry has one.
    ///
    /// **Why**: `send_draft_email` silently attaches this PDF to outbound
    /// drafts in the thread (see below). Admins had no way to see *before*
    /// sending that an attachment would go out — this surfaces it in the UI
    /// so a draft that says "please find attached..." actually shows one.
    offer_pdf_filename: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(super) struct EmailMessageItem {
    id: Uuid,
    direction: String,
    from_address: String,
    to_address: String,
    subject: Option<String>,
    body_text: Option<String>,
    llm_generated: bool,
    status: String,
    attachment_keys: Vec<String>,
    created_at: DateTime<Utc>,
}

/// `GET /api/v1/admin/emails/{id}` — Return an email thread with all its messages.
///
/// **Caller**: Axum router / admin dashboard email thread detail page.
/// **Why**: Returns the thread header and all non-discarded messages in chronological order.
/// Draft messages are included so the admin can review before sending.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — thread UUID path parameter
///
/// # Returns
/// `200 OK` with `EmailThreadDetailResponse` (thread + messages array).
///
/// # Errors
/// - `404` if thread not found
pub(super) async fn get_email_thread(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<EmailThreadDetailResponse>, ApiError> {
    let repo_thread = admin_repo::fetch_email_thread(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("E-Mail-Thread {id} nicht gefunden")))?;

    let offer_pdf_filename = match repo_thread.inquiry_id {
        Some(inquiry_id) => fetch_offer_pdf_filename(&state.db, inquiry_id).await?,
        None => None,
    };

    let thread = EmailThreadDetail {
        id: repo_thread.id, customer_id: repo_thread.customer_id,
        customer_email: repo_thread.customer_email, customer_name: repo_thread.customer_name,
        inquiry_id: repo_thread.inquiry_id, subject: repo_thread.subject,
        offer_pdf_filename, created_at: repo_thread.created_at,
    };

    let repo_messages = admin_repo::fetch_thread_messages(&state.db, id).await?;
    let messages: Vec<EmailMessageItem> = repo_messages
        .into_iter()
        .map(|m| EmailMessageItem {
            id: m.id, direction: m.direction, from_address: m.from_address,
            to_address: m.to_address, subject: m.subject, body_text: m.body_text,
            llm_generated: m.llm_generated, status: m.status,
            attachment_keys: m.attachment_keys, created_at: m.created_at,
        })
        .collect();

    Ok(Json(EmailThreadDetailResponse { thread, messages }))
}

/// `POST /api/v1/admin/emails/messages/{id}/send` — Send a draft email via SMTP.
///
/// **Caller**: Axum router / admin dashboard "Senden" button in the email thread view.
/// **Why**: Fetches the draft message body and the customer's real email (via the thread →
/// customer join), sends via SMTP, and marks the message as `sent`. The `to_address` is
/// corrected to the real customer email (overriding whatever placeholder was stored).
///
/// # Parameters
/// - `state` — shared AppState (DB pool, SMTP config)
/// - `id` — email_message UUID path parameter (must have `status = 'draft'`)
///
/// # Returns
/// `200 OK` with `{"message": "E-Mail an <email> gesendet"}`.
///
/// # Errors
/// - `404` if the draft message does not exist or is not in draft status
/// - `500` on SMTP failures
pub(super) async fn send_draft_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Fetch draft + customer email + optional offer PDF key (when thread belongs to an inquiry with an active offer)
    let row = admin_repo::fetch_draft_for_send(&state.db, id).await?;

    let (subject, body_text, customer_email, pdf_key, offer_id, inquiry_id) =
        row.ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden oder bereits gesendet".into()))?;

    let subject = subject.unwrap_or_else(|| "Ihr Umzugsangebot — AUST Umzüge".into());
    let body = body_text.unwrap_or_default();

    // If the thread is tied to an inquiry with a PDF offer, send with attachment
    if let (Some(key), Some(oid), Some(iid)) = (&pdf_key, offer_id, inquiry_id) {
        use crate::repositories::offer_repo;
        use crate::services::email::{build_email_with_attachment, send_email};

        let pdf_bytes = state
            .storage
            .download(key)
            .await
            .map_err(|e| match e {
                aust_storage::StorageError::NotFound(_) => {
                    ApiError::NotFound("Angebot-PDF nicht gefunden.".into())
                }
                _ => ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")),
            })?;

        let attach_filename = if let Ok(Some((offer_num, last_name))) =
            offer_repo::fetch_offer_filename_parts(&state.db, oid).await
        {
            offer_repo::build_offer_filename(&offer_num, &last_name, "pdf")
        } else {
            format!("Angebot-{oid}.pdf")
        };

        let email_cfg = &state.config.email;
        let message = build_email_with_attachment(
            &email_cfg.from_address,
            &email_cfg.from_name,
            &customer_email,
            &subject,
            &body,
            &pdf_bytes,
            &attach_filename,
            "application/pdf",
        )
        .map_err(|e| ApiError::Internal(format!("E-Mail-Aufbau fehlgeschlagen: {e}")))?;

        send_email(
            &email_cfg.smtp_host,
            email_cfg.smtp_port,
            &email_cfg.smtp_tls,
            &email_cfg.username,
            &email_cfg.password,
            message,
        )
        .await
        .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;

        let now = chrono::Utc::now();

        // Update offer and inquiry status
        admin_repo::mark_offer_sent(&state.db, oid, now).await?;
        admin_repo::mark_inquiry_offer_sent(&state.db, iid, now).await?;

        // Emit offer.sent domain event (non-fatal).
        {
            let emitter = state.events.clone();
            let payload = serde_json::json!({
                "offer_id": oid,
                "inquiry_id": iid,
            });
            let aggregate = format!("offer:{oid}");
            tokio::spawn(async move {
                if let Err(e) = emitter.emit("offer.sent", &aggregate, payload).await {
                    tracing::warn!("Failed to emit offer.sent event: {e}");
                }
            });
        }
    } else {
        // Plain email — no offer PDF attached (e.g. general inquiry reply)
        send_plain_email(&state.config.email, &customer_email, &subject, &body)
            .await
            .map_err(|e| ApiError::Internal(format!("E-Mail-Versand fehlgeschlagen: {e}")))?;
    }

    // Mark draft as sent + fix to_address
    admin_repo::mark_message_sent(&state.db, id, &customer_email).await?;

    Ok(Json(serde_json::json!({
        "message": format!("E-Mail an {customer_email} gesendet"),
    })))
}

/// `POST /api/v1/admin/emails/messages/{id}/discard` — Discard a draft email.
///
/// **Caller**: Axum router / admin dashboard "Verwerfen" button in the email thread view.
/// **Why**: Sets `email_messages.status = 'discarded'` so the draft is excluded from the
/// thread view without being physically deleted. Prevents accidental sends of stale drafts.
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — email_message UUID path parameter (must have `status = 'draft'`)
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if draft not found or already processed
pub(super) async fn discard_draft_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = admin_repo::discard_draft(&state.db, id).await?;
    if rows == 0 {
        return Err(ApiError::NotFound("Entwurf nicht gefunden oder bereits verarbeitet".into()));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Edit Draft Content ---

#[derive(Debug, Deserialize)]
pub(super) struct UpdateDraftRequest {
    subject: Option<String>,
    body_text: Option<String>,
}

/// `PATCH /api/v1/admin/emails/messages/{id}` — Edit the subject or body of a draft email.
///
/// **Caller**: Axum router / admin dashboard email draft editor.
/// **Why**: Allows Alex to tweak the LLM-generated draft before sending. Only drafts can
/// be edited (status check via `WHERE status = 'draft'`).
///
/// # Parameters
/// - `state` — shared AppState (DB pool)
/// - `id` — email_message UUID path parameter
/// - `request` — optional `subject` and/or `body_text` fields to overwrite
///
/// # Returns
/// `200 OK` with `{"ok": true}`.
///
/// # Errors
/// - `404` if draft not found or already sent/discarded
pub(super) async fn update_draft_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateDraftRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = admin_repo::update_draft(&state.db, id, request.subject.as_deref(), request.body_text.as_deref()).await?;
    if rows == 0 {
        return Err(ApiError::NotFound(
            "Entwurf nicht gefunden oder bereits gesendet".into(),
        ));
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Reply to Thread ---

#[derive(Debug, Deserialize)]
pub(super) struct ReplyRequest {
    subject: Option<String>,
    body_text: String,
}

/// `POST /api/v1/admin/emails/{id}/reply` — Create a new draft reply in an existing thread.
///
/// **Caller**: Axum router / admin dashboard thread reply composer.
/// **Why**: Inserts a new outbound `email_messages` row in `draft` status tied to the
/// existing thread, without sending it immediately. The admin then uses `send_draft_email`
/// to approve and send.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, email config for `from_address`)
/// - `thread_id` — thread UUID path parameter
/// - `request` — `body_text` (required) and optional `subject` override
///
/// # Returns
/// `201 Created` with `{"id": ..., "status": "draft"}`.
///
/// # Errors
/// - `404` if thread not found
pub(super) async fn reply_to_thread(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(thread_id): Path<Uuid>,
    Json(request): Json<ReplyRequest>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), ApiError> {
    let row = admin_repo::fetch_thread_for_reply(&state.db, thread_id).await?;
    let (_customer_id, customer_email, thread_subject) = row.ok_or_else(|| {
        ApiError::NotFound(format!("E-Mail-Thread {thread_id} nicht gefunden"))
    })?;

    let subject = request.subject.or(thread_subject);
    let from_address = &state.config.email.from_address;
    let id = Uuid::now_v7();
    let now = Utc::now();

    admin_repo::insert_reply_draft(
        &state.db, id, thread_id, from_address, &customer_email,
        subject.as_deref(), &request.body_text, now,
    )
    .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "status": "draft",
        })),
    ))
}

// --- Compose New Email ---

#[derive(Debug, Deserialize)]
pub(super) struct ComposeEmailRequest {
    customer_email: String,
    subject: String,
    body_text: String,
}

/// `POST /api/v1/admin/emails/compose` — Compose a new outbound email to any address.
///
/// **Caller**: Axum router / admin dashboard "Neue E-Mail" compose button.
/// **Why**: Creates a new thread (upserts the customer by email) and a draft message in
/// one operation, allowing the admin to initiate contact with a customer not yet in the
/// system. The draft is saved and can be reviewed before sending via `send_draft_email`.
///
/// # Parameters
/// - `state` — shared AppState (DB pool, email config for `from_address`)
/// - `request` — `customer_email`, `subject`, `body_text` (all required)
///
/// # Returns
/// `201 Created` with `{"thread_id": ..., "message_id": ...}`.
pub(super) async fn compose_email(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Json(request): Json<ComposeEmailRequest>,
) -> Result<(axum::http::StatusCode, Json<serde_json::Value>), ApiError> {
    let now = Utc::now();

    // Upsert customer by email
    let customer_id = admin_repo::upsert_customer_for_compose(&state.db, &request.customer_email, now).await?;

    // Create thread
    let thread_id = Uuid::now_v7();
    admin_repo::create_compose_thread(&state.db, thread_id, customer_id, &request.subject, now).await?;

    // Create draft message
    let message_id = Uuid::now_v7();
    let from_address = &state.config.email.from_address;
    admin_repo::insert_compose_draft(
        &state.db, message_id, thread_id, from_address,
        &request.customer_email, &request.subject, &request.body_text, now,
    )
    .await?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(serde_json::json!({
            "thread_id": thread_id,
            "message_id": message_id,
        })),
    ))
}

/// Resolve the display filename of an inquiry's active offer PDF, if one was generated.
///
/// **Caller**: `get_email_thread`
/// **Why**: Mirrors the PDF lookup `send_draft_email` already does at send-time
/// (`offer_repo::fetch_active_pdf_key` + `fetch_offer_filename_parts` +
/// `build_offer_filename`), so the thread view can show the same attachment
/// before the admin hits "Senden" instead of only after.
async fn fetch_offer_pdf_filename(
    pool: &sqlx::PgPool,
    inquiry_id: Uuid,
) -> Result<Option<String>, ApiError> {
    let Some((offer_id, Some(_storage_key))) =
        offer_repo::fetch_active_pdf_key(pool, inquiry_id).await?
    else {
        return Ok(None);
    };

    let filename = match offer_repo::fetch_offer_filename_parts(pool, offer_id).await? {
        Some((offer_num, last_name)) => offer_repo::build_offer_filename(&offer_num, &last_name, "pdf"),
        None => format!("Angebot-{offer_id}.pdf"),
    };
    Ok(Some(filename))
}

/// `GET /api/v1/admin/emails/messages/{id}/attachments/{idx}` — Download one attachment
/// of an email message by index (admin only).
///
/// **Caller**: Admin email thread detail view — attachment preview/download links.
/// **Why**: Mirrors `download_feedback_attachment` (`routes/admin.rs`) — proxies the
/// attachment from S3 with the correct content-disposition header rather than exposing
/// bucket URLs to the frontend.
///
/// # Path Parameters
/// - `id`  — email_message UUID
/// - `idx` — zero-based attachment index
///
/// # Returns
/// Binary response with `Content-Disposition: attachment` header, or `404`.
pub(super) async fn download_message_attachment(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path((id, idx)): Path<(Uuid, usize)>,
) -> Result<Response, ApiError> {
    let keys = admin_repo::fetch_message_attachment_keys(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Nachricht nicht gefunden.".into()))?;

    let key = keys
        .get(idx)
        .ok_or_else(|| ApiError::NotFound("Anhang nicht gefunden.".into()))?;

    let data = state.storage.download(key).await.map_err(|e| match e {
        aust_storage::StorageError::NotFound(_) => {
            tracing::warn!("Email attachment not found in storage: {key}");
            ApiError::NotFound("Anhang nicht gefunden.".into())
        }
        _ => {
            tracing::error!("S3 download for email attachment {key}: {e}");
            ApiError::NotFound("Anhang konnte nicht abgerufen werden.".into())
        }
    })?;

    let filename = key.rsplit('/').next().unwrap_or("attachment");
    let ext = filename.rsplit('.').next().unwrap_or("bin");
    let ct = mime_from_ext(ext);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, ct)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(axum::body::Body::from(data))
        .unwrap())
}

/// Send a plain-text email via SMTP using the configured outbound email credentials.
///
/// **Caller**: `send_draft_email` — the only SMTP send path in the admin emails module.
/// **Why**: Thin wrapper around `services::email::{build_plain_email, send_email}` so the
/// SMTP credentials from `Config.email` stay out of individual route handlers.
///
/// # Parameters
/// - `email_config` — SMTP host/port/credentials and from_address/from_name
/// - `to` — recipient email address
/// - `subject` — email subject line
/// - `body` — plain-text body
///
/// # Errors
/// Returns `Err(String)` describing the failure if building the message or the SMTP
/// transmission fails.
pub(crate) async fn send_plain_email(
    email_config: &aust_core::config::EmailConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use crate::services::email::{build_plain_email, send_email};

    let message = build_plain_email(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
    )
    .map_err(|e| format!("Failed to build email: {e}"))?;

    send_email(
        &email_config.smtp_host,
        email_config.smtp_port,
        &email_config.smtp_tls,
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())
}
