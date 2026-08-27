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
use crate::repositories::{admin_repo, email_repo, offer_repo};
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
    customer_id: Option<Uuid>,
    customer_email: Option<String>,
    customer_name: Option<String>,
    inquiry_id: Option<Uuid>,
    subject: Option<String>,
    message_count: i64,
    unread_count: i64,
    unhandled_count: i64,
    muted: bool,
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
            message_count: t.message_count, unread_count: t.unread_count,
            unhandled_count: t.unhandled_count, muted: t.muted,
            last_message_at: t.last_message_at,
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
    customer_id: Option<Uuid>,
    /// Thread opted out of the unanswered-email Telegram nag.
    muted: bool,
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
    cc_addresses: Vec<String>,
    subject: Option<String>,
    body_text: Option<String>,
    /// Sanitised HTML body, `None` when the mail had none.
    ///
    /// **Why sanitised here and not on the way in**: the raw MIME part is kept in
    /// `email_messages.body_html` as evidence of what the customer actually sent;
    /// what the dashboard renders is a scrubbed copy. Ammonia strips scripts,
    /// event handlers, iframes, forms and `style` blocks, and remote images are
    /// dropped rather than fetched so opening a mail cannot phone home to a
    /// tracking pixel.
    body_html: Option<String>,
    llm_generated: bool,
    status: String,
    read_at: Option<DateTime<Utc>>,
    handled_at: Option<DateTime<Utc>>,
    attachment_keys: Vec<String>,
    attachment_names: Vec<String>,
    created_at: DateTime<Utc>,
}

/// Scrub an inbound HTML body down to something safe to drop into the dashboard.
///
/// **Caller**: `get_email_thread`
/// **Why**: an email body is attacker-controlled. Beyond the obvious script
/// removal, `url_relative(Deny)` and stripping `img` mean a marketing mail cannot
/// silently report that Alex opened it.
fn sanitize_email_html(html: &str) -> String {
    use std::collections::HashSet;
    ammonia::Builder::default()
        .rm_tags(["img"])
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(Some("noopener noreferrer nofollow"))
        .generic_attributes(HashSet::from(["style"]))
        .clean(html)
        .to_string()
}

#[derive(Debug, Deserialize)]
pub(super) struct LinkThreadInquiryRequest {
    inquiry_id: Uuid,
}

/// `PATCH /api/v1/admin/emails/{id}/inquiry` — Attach an email thread to an inquiry.
///
/// **Caller**: Admin email thread page, after creating an inquiry straight from a
/// customer mail.
/// **Why**: Alex could not turn an incoming mail into an Anfrage without leaving the
/// mailbox, copying the address by hand and losing the conversation link (feedback
/// report 71e097f6). The thread carries the customer; only the inquiry link was
/// missing, and it already exists as a column on `email_threads`.
pub(super) async fn link_thread_to_inquiry(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(req): Json<LinkThreadInquiryRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    admin_repo::fetch_email_thread(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("E-Mail-Thread {id} nicht gefunden")))?;

    email_repo::link_thread_to_inquiry(&state.db, id, req.inquiry_id).await?;

    Ok(Json(serde_json::json!({ "inquiry_id": req.inquiry_id })))
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
        muted: repo_thread.muted,
        customer_email: repo_thread.customer_email, customer_name: repo_thread.customer_name,
        inquiry_id: repo_thread.inquiry_id, subject: repo_thread.subject,
        offer_pdf_filename, created_at: repo_thread.created_at,
    };

    let repo_messages = admin_repo::fetch_thread_messages(&state.db, id).await?;

    // Read the messages first, then stamp them read: the response should still show
    // which ones *were* unread when the thread was opened, so the UI can mark them
    // without them silently reappearing as read on this very request.
    let messages: Vec<EmailMessageItem> = repo_messages
        .into_iter()
        .map(|m| EmailMessageItem {
            id: m.id, direction: m.direction, from_address: m.from_address,
            to_address: m.to_address, cc_addresses: m.cc_addresses,
            subject: m.subject, body_text: m.body_text,
            body_html: m.body_html.as_deref().map(sanitize_email_html),
            llm_generated: m.llm_generated, status: m.status,
            read_at: m.read_at, handled_at: m.handled_at,
            attachment_keys: m.attachment_keys, attachment_names: m.attachment_names,
            created_at: m.created_at,
        })
        .collect();

    // Opening a thread is the read event — non-fatal, a failed stamp only means the
    // badge stays stale, and losing the thread view over it would be worse.
    if let Err(e) = admin_repo::mark_thread_read(&state.db, id).await {
        tracing::warn!("Failed to mark thread {id} read: {e}");
    }

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
    use crate::repositories::offer_repo;
    use crate::services::email::{build_message, send_email, OutboundAttachment, OutboundEmail};

    let draft = admin_repo::fetch_draft_for_send(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden oder bereits gesendet".into()))?;

    // A thread can exist with no customer, no contact address and no inbound mail
    // (a compose draft whose recipient was cleared). Refusing here beats handing
    // lettre an empty string and reporting an opaque SMTP error.
    let to_address = draft.to_address.filter(|a| a.contains('@')).ok_or_else(|| {
        ApiError::BadRequest("Kein Empfänger hinterlegt — bitte Adresse ergänzen.".into())
    })?;

    let subject = draft
        .subject
        .unwrap_or_else(|| "Ihr Umzugsangebot — AUST Umzüge".into());
    let body = draft.body_text.unwrap_or_default();

    let mut attachments: Vec<OutboundAttachment> = Vec::new();

    // The thread's active offer PDF, when there is one. Unchanged behaviour — it is
    // attached implicitly, which is why the thread view surfaces its filename.
    let offer_ids = match (&draft.pdf_storage_key, draft.offer_id, draft.inquiry_id) {
        (Some(key), Some(oid), Some(iid)) => {
            let pdf_bytes = state.storage.download(key).await.map_err(|e| match e {
                aust_storage::StorageError::NotFound(_) => {
                    ApiError::NotFound("Angebot-PDF nicht gefunden.".into())
                }
                _ => ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")),
            })?;

            let filename = match offer_repo::fetch_offer_filename_parts(&state.db, oid).await {
                Ok(Some((offer_num, last_name))) => {
                    offer_repo::build_offer_filename(&offer_num, &last_name, "pdf")
                }
                _ => format!("Angebot-{oid}.pdf"),
            };

            attachments.push(OutboundAttachment {
                filename,
                content_type: "application/pdf".into(),
                data: pdf_bytes.to_vec(),
            });
            Some((oid, iid))
        }
        _ => None,
    };

    // Files the admin attached by hand in the composer. The active offer PDF is already
    // attached implicitly above, so attaching it again from the document picker must not
    // put two copies of the same KVA in the mail.
    let implicit_key = draft.pdf_storage_key.as_deref();
    for (key, filename) in admin_repo::fetch_message_attachments(&state.db, id).await? {
        if Some(key.as_str()) == implicit_key {
            continue;
        }
        let data = state.storage.download(&key).await.map_err(|e| {
            ApiError::Internal(format!("Anhang '{filename}' konnte nicht geladen werden: {e}"))
        })?;
        let ext = filename.rsplit('.').next().unwrap_or("bin");
        attachments.push(OutboundAttachment {
            filename: filename.clone(),
            content_type: crate::routes::admin::mime_from_ext(ext).to_string(),
            data: data.to_vec(),
        });
    }

    let email_cfg = &state.config.email;
    let message = build_message(&OutboundEmail {
        from_address: &email_cfg.from_address,
        from_name: &email_cfg.from_name,
        to: &to_address,
        cc: &draft.cc_addresses,
        bcc: &draft.bcc_addresses,
        subject: &subject,
        body: &body,
        // Thread the reply into the customer's existing conversation. The admin send
        // path never set this, so every reply from the dashboard opened a new thread
        // on their side even though the agent's own SMTP client had done it correctly
        // for years.
        in_reply_to: draft.parent_message_id.as_deref(),
        attachments,
    })
    .map_err(ApiError::BadRequest)?;

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

    // Only advance the offer/inquiry state once the mail is actually out.
    if let Some((oid, iid)) = offer_ids {
        let now = chrono::Utc::now();
        admin_repo::mark_offer_sent(&state.db, oid, now).await?;
        admin_repo::mark_inquiry_offer_sent(&state.db, iid, now).await?;

        let emitter = state.events.clone();
        let payload = serde_json::json!({ "offer_id": oid, "inquiry_id": iid });
        let aggregate = format!("offer:{oid}");
        tokio::spawn(async move {
            if let Err(e) = emitter.emit("offer.sent", &aggregate, payload).await {
                tracing::warn!("Failed to emit offer.sent event: {e}");
            }
        });
    }

    admin_repo::mark_message_sent(&state.db, id, &to_address).await?;

    // Answering the thread is what "erledigt" means; the Telegram nag stops here
    // rather than waiting for someone to tick the inbound mail off by hand.
    if let Err(e) = admin_repo::mark_thread_handled(&state.db, draft.thread_id).await {
        tracing::warn!("Failed to mark thread {} handled: {e}", draft.thread_id);
    }

    Ok(Json(serde_json::json!({
        "message": format!("E-Mail an {to_address} gesendet"),
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
    to_address: Option<String>,
    cc: Option<Vec<String>>,
    bcc: Option<Vec<String>>,
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

    // Recipients are updated separately and only when the request mentions them, so
    // an edit that touches just the body cannot silently drop the CC list.
    if request.to_address.is_some() || request.cc.is_some() || request.bcc.is_some() {
        admin_repo::set_draft_recipients(
            &state.db,
            id,
            request.to_address.as_deref(),
            &request.cc.unwrap_or_default(),
            &request.bcc.unwrap_or_default(),
        )
        .await?;
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Reply to Thread ---

#[derive(Debug, Deserialize)]
pub(super) struct ReplyRequest {
    subject: Option<String>,
    body_text: String,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
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
    let (_customer_id, recipient, thread_subject) = row.ok_or_else(|| {
        ApiError::NotFound(format!("E-Mail-Thread {thread_id} nicht gefunden"))
    })?;

    // A customer-less thread still has a recipient (contact_address or the last
    // inbound sender); only a thread that has never had any address is unanswerable.
    let recipient = recipient.unwrap_or_default();

    let subject = request.subject.or(thread_subject);
    let from_address = &state.config.email.from_address;
    let id = Uuid::now_v7();
    let now = Utc::now();

    admin_repo::insert_reply_draft(
        &state.db, id, thread_id, from_address, &recipient,
        subject.as_deref(), &request.body_text, now,
    )
    .await?;

    if !request.cc.is_empty() || !request.bcc.is_empty() {
        admin_repo::set_draft_recipients(&state.db, id, None, &request.cc, &request.bcc).await?;
    }

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
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
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

    if !request.cc.is_empty() || !request.bcc.is_empty() {
        admin_repo::set_draft_recipients(&state.db, message_id, None, &request.cc, &request.bcc)
            .await?;
    }

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
    // Pairs each key with its display name, falling back to the key's basename for
    // rows written before `attachment_names` existed.
    let attachments = admin_repo::fetch_message_attachments(&state.db, id).await?;

    let (key, display_name) = attachments
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

    let filename = display_name.as_str();
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
    use crate::services::email::{build_message, send_email, OutboundEmail};

    let message = build_message(&OutboundEmail::new(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
    ))?;

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

// --- Mailbox state: read / handled / muted ---

#[derive(Debug, Deserialize)]
pub(super) struct SetHandledRequest {
    handled: bool,
}

/// `PATCH /api/v1/admin/emails/messages/{id}/handled` — Tick an inbound mail off.
///
/// **Caller**: Admin email thread view, "Erledigt" toggle on an inbound message.
/// **Why**: `handled_at IS NULL` is the condition the assistant's unanswered-email
/// reminder reconciles against, so this is what actually silences a nag. It is kept
/// distinct from read state on purpose — having seen a mail is not having answered it.
pub(super) async fn set_message_handled(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetHandledRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ok = admin_repo::set_message_handled(&state.db, id, req.handled).await?;
    if !ok {
        return Err(ApiError::NotFound(
            "Eingehende Nachricht nicht gefunden".into(),
        ));
    }
    Ok(Json(serde_json::json!({ "handled": req.handled })))
}

#[derive(Debug, Deserialize)]
pub(super) struct SetMutedRequest {
    muted: bool,
}

/// `PATCH /api/v1/admin/emails/{id}/mute` — Silence a thread's reminders.
///
/// **Caller**: Admin email thread view, "Stummschalten".
/// **Why**: a newsletter or an automated notification thread would otherwise nag
/// forever, and the only way to stop it was to mark its mail handled — claiming
/// something was answered when it was not.
pub(super) async fn set_thread_muted(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetMutedRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ok = admin_repo::set_thread_muted(&state.db, id, req.muted).await?;
    if !ok {
        return Err(ApiError::NotFound(format!(
            "E-Mail-Thread {id} nicht gefunden"
        )));
    }
    Ok(Json(serde_json::json!({ "muted": req.muted })))
}

/// `GET /api/v1/admin/emails/unread` — Badge counts for the mailbox.
///
/// **Caller**: Admin shell navigation.
/// **Why**: nothing in the dashboard indicated that mail had arrived; Alex had to
/// open the E-Mails tab to find out.
pub(super) async fn email_unread_counts(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (unread_messages, unread_threads, unhandled) =
        admin_repo::email_unread_counts(&state.db).await?;
    Ok(Json(serde_json::json!({
        "unread_messages": unread_messages,
        "unread_threads": unread_threads,
        "unhandled": unhandled,
    })))
}

/// `POST /api/v1/admin/emails/messages/{id}/attachments` — Attach a file to a draft.
///
/// **Caller**: Admin email composer, "Datei anhängen".
/// **Why**: outbound mail could carry exactly one attachment, the offer PDF, and only
/// because the send path hardcoded it. Anything else — a floor plan, a signed
/// contract, a photo — had to be sent from a separate mail client, which took the
/// conversation out of the system entirely.
///
/// Stored under `emails/{thread}/{message}/…` like inbound attachments, so both
/// directions download through the same route.
pub(super) async fn upload_draft_attachment(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

    let thread_id = admin_repo::fetch_message_thread_id(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden".into()))?;

    let mut stored = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Ungültiger Upload: {e}")))?
    {
        let filename = field
            .file_name()
            .map(sanitize_filename)
            .unwrap_or_else(|| "anhang".to_string());
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::BadRequest(format!("Anhang konnte nicht gelesen werden: {e}")))?;

        if data.len() > MAX_ATTACHMENT_BYTES {
            return Err(ApiError::BadRequest(format!(
                "Anhang '{filename}' ist größer als 20 MB."
            )));
        }

        let idx = stored.len();
        let ext = filename.rsplit('.').next().unwrap_or("bin");
        let key = format!("emails/{thread_id}/{id}/out-{idx}-{}.{ext}", Uuid::now_v7());

        state
            .storage
            .upload(&key, data, &content_type)
            .await
            .map_err(|e| ApiError::Internal(format!("Upload fehlgeschlagen: {e}")))?;

        admin_repo::append_draft_attachment(&state.db, id, &key, &filename).await?;
        stored.push(filename);
    }

    if stored.is_empty() {
        return Err(ApiError::BadRequest("Keine Datei übermittelt.".into()));
    }

    Ok(Json(serde_json::json!({ "attachments": stored })))
}

/// Strip path separators out of a client-supplied filename.
///
/// **Caller**: `upload_draft_attachment`
/// **Why**: the name goes into an S3 key and into a `Content-Disposition` header.
/// A name like `../../etc/passwd` must not be able to steer either.
fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("anhang");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '"')
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "anhang".to_string()
    } else {
        trimmed.to_string()
    }
}

/// One ready-made document (KVA or Rechnung) offered by the draft composer.
#[derive(Debug, Serialize)]
pub(super) struct ThreadDocumentItem {
    /// `"offer"` or `"invoice"` — the discriminator the attach call sends back.
    kind: String,
    id: Uuid,
    label: String,
    filename: String,
    created_at: DateTime<Utc>,
    /// True when this exact file is already hung on the draft being composed.
    attached: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct AttachDocumentRequest {
    kind: String,
    id: Uuid,
}

/// `GET /api/v1/admin/emails/{id}/documents` — Documents attachable to a draft in this thread.
///
/// **Caller**: Admin email composer, "KVA/Rechnung anhängen" picker.
/// **Why**: the composer could only attach files from the admin's own disk, so sending a
/// KVA or an invoice meant downloading it from the dashboard and uploading it straight
/// back. Only documents whose PDF actually exists are listed — an offer that was never
/// generated is not ready to send.
///
/// The optional `message` query parameter is the draft being composed; when given, each
/// entry reports whether it is already attached to that draft.
pub(super) async fn list_thread_documents(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Query(query): Query<ThreadDocumentsQuery>,
) -> Result<Json<Vec<ThreadDocumentItem>>, ApiError> {
    let thread = admin_repo::fetch_email_thread(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("E-Mail-Thread {id} nicht gefunden")))?;

    let docs =
        admin_repo::fetch_thread_documents(&state.db, thread.customer_id, thread.inquiry_id).await?;

    // Keys already on the draft, so the picker can grey out what is on there twice.
    let attached: Vec<String> = match query.message {
        Some(msg_id) => admin_repo::fetch_message_attachments(&state.db, msg_id)
            .await?
            .into_iter()
            .map(|(key, _)| key)
            .collect(),
        None => Vec::new(),
    };

    Ok(Json(
        docs.into_iter()
            .map(|d| ThreadDocumentItem {
                attached: attached.iter().any(|k| k == &d.storage_key),
                kind: d.kind,
                id: d.id,
                label: d.label,
                filename: d.filename,
                created_at: d.created_at,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct ThreadDocumentsQuery {
    /// Draft message id, used only to flag documents already attached to it.
    message: Option<Uuid>,
}

/// `POST /api/v1/admin/emails/messages/{id}/attachments/document` — Attach a KVA or Rechnung.
///
/// **Caller**: Admin email composer, document picker.
/// **Why**: hangs an already-generated PDF on the draft by reference. The stored S3 key is
/// reused rather than copied — the send path and the download route both read the draft's
/// keys straight from storage, so a second copy would only be a second thing to keep in
/// sync.
///
/// # Errors
/// - `404` if the draft does not exist (or is no longer a draft), or the document is not
///   one of this thread's
/// - `400` if the document is already attached
pub(super) async fn attach_thread_document(
    State(state): State<Arc<AppState>>,
    Extension(_claims): Extension<TokenClaims>,
    Path(id): Path<Uuid>,
    Json(req): Json<AttachDocumentRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let thread_id = admin_repo::fetch_message_thread_id(&state.db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden".into()))?;

    let thread = admin_repo::fetch_email_thread(&state.db, thread_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("E-Mail-Thread nicht gefunden".into()))?;

    // Resolving through the same list the picker was built from is what keeps this
    // route from being a "download any storage key by id" hole.
    let docs =
        admin_repo::fetch_thread_documents(&state.db, thread.customer_id, thread.inquiry_id).await?;
    let doc = docs
        .into_iter()
        .find(|d| d.id == req.id && d.kind == req.kind)
        .ok_or_else(|| ApiError::NotFound("Dokument nicht gefunden.".into()))?;

    let already = admin_repo::fetch_message_attachments(&state.db, id).await?;
    if already.iter().any(|(key, _)| key == &doc.storage_key) {
        return Err(ApiError::BadRequest(format!(
            "'{}' hängt bereits am Entwurf.",
            doc.filename
        )));
    }

    // Fail here rather than at send time: a missing object is worth knowing about
    // while the admin is still looking at the composer.
    state.storage.download(&doc.storage_key).await.map_err(|e| match e {
        aust_storage::StorageError::NotFound(_) => {
            ApiError::NotFound(format!("'{}' liegt nicht im Speicher.", doc.filename))
        }
        other => ApiError::Internal(format!("Dokument konnte nicht geladen werden: {other}")),
    })?;

    admin_repo::append_draft_attachment(&state.db, id, &doc.storage_key, &doc.filename).await?;

    Ok(Json(serde_json::json!({ "attachment": doc.filename })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeds a thread plus one inbound message and returns both ids.
    ///
    /// `customer` is optional on purpose — a thread with no customer is the case
    /// that used to lose mail outright, so the tests need to be able to build one.
    async fn seed_thread(
        pool: &sqlx::PgPool,
        customer_id: Option<Uuid>,
        from_address: &str,
    ) -> (Uuid, Uuid) {
        let thread_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO email_threads (id, customer_id, contact_address, subject, created_at, updated_at) \
             VALUES ($1, $2, $3, 'Testbetreff', NOW(), NOW())",
        )
        .bind(thread_id)
        .bind(customer_id)
        .bind(from_address)
        .execute(pool)
        .await
        .expect("insert thread");

        let message_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO email_messages \
             (id, thread_id, direction, from_address, to_address, subject, body_text, \
              message_id, llm_generated, status, created_at) \
             VALUES ($1, $2, 'inbound', $3, 'angebot@aust-umzuege.de', 'Testbetreff', \
                     'Hallo', $4, false, 'received', NOW())",
        )
        .bind(message_id)
        .bind(thread_id)
        .bind(from_address)
        .bind(format!("rfc-{message_id}@example.com"))
        .execute(pool)
        .await
        .expect("insert message");

        (thread_id, message_id)
    }

    async fn message_state(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
        sqlx::query_as("SELECT read_at, handled_at FROM email_messages WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("fetch message state")
    }

    #[tokio::test]
    async fn opening_a_thread_marks_it_read_but_not_handled() {
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let (thread_id, message_id) = seed_thread(&pool, None, "kunde@example.com").await;

        assert_eq!(message_state(&pool, message_id).await, (None, None));

        admin_repo::mark_thread_read(&pool, thread_id)
            .await
            .expect("mark read");

        let (read_at, handled_at) = message_state(&pool, message_id).await;
        assert!(read_at.is_some(), "opening the thread should mark it read");
        assert!(
            handled_at.is_none(),
            "having read a mail is not having answered it — the nag must survive"
        );
    }

    #[tokio::test]
    async fn answering_a_thread_marks_its_inbound_mail_handled() {
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let (thread_id, message_id) = seed_thread(&pool, None, "kunde@example.com").await;

        admin_repo::mark_thread_handled(&pool, thread_id)
            .await
            .expect("mark handled");

        let (read_at, handled_at) = message_state(&pool, message_id).await;
        assert!(handled_at.is_some(), "replying is handling it");
        assert!(read_at.is_some(), "and implies it was read");
    }

    #[tokio::test]
    async fn a_muted_thread_reports_itself_as_muted_with_its_counts_intact() {
        // Scoped to this thread by a unique contact address: the shared test database
        // runs these in parallel, so any assertion against a global count is a race.
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let marker = format!("mute-{}@example.com", Uuid::now_v7());
        let (thread_id, _) = seed_thread(&pool, None, &marker).await;

        assert!(
            admin_repo::set_thread_muted(&pool, thread_id, true)
                .await
                .expect("mute"),
            "muting an existing thread should report a hit"
        );

        let listed = admin_repo::list_email_threads(&pool, &marker, 10, 0)
            .await
            .expect("list");
        let row = listed
            .iter()
            .find(|t| t.id == thread_id)
            .expect("thread is findable by its contact address");

        assert!(row.muted, "the list must surface the mute so the UI can show it");
        assert_eq!(
            row.unhandled_count, 1,
            "muting silences the nag; it does not pretend the mail was answered"
        );
        assert_eq!(row.unread_count, 1);
    }

    #[tokio::test]
    async fn a_thread_is_searchable_by_message_body() {
        // Search used to cover the customer and the subject only, so a thread whose
        // subject said nothing useful was unfindable.
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let (thread_id, message_id) = seed_thread(&pool, None, "kunde@example.com").await;

        let needle = format!("Klaviertransport-{}", Uuid::now_v7().simple());
        sqlx::query("UPDATE email_messages SET body_text = $2 WHERE id = $1")
            .bind(message_id)
            .bind(format!("Guten Tag, Frage zum {needle}."))
            .execute(&pool)
            .await
            .expect("set body");

        let listed = admin_repo::list_email_threads(&pool, &format!("%{needle}%"), 10, 0)
            .await
            .expect("list");

        assert!(
            listed.iter().any(|t| t.id == thread_id),
            "a word that appears only in the body must find the thread"
        );
    }

    #[tokio::test]
    async fn a_customerless_thread_still_resolves_a_recipient() {
        // The exact shape that used to be dropped on the floor: mail we could not
        // attribute to anyone. It must still be answerable.
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let (thread_id, _) = seed_thread(&pool, None, "unbekannt@example.com").await;

        let row = admin_repo::fetch_thread_for_reply(&pool, thread_id)
            .await
            .expect("fetch")
            .expect("thread exists");

        let (customer_id, recipient, _subject) = row;
        assert!(customer_id.is_none(), "seeded without a customer");
        assert_eq!(recipient.as_deref(), Some("unbekannt@example.com"));
    }

    #[tokio::test]
    async fn handled_can_be_toggled_back_off() {
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let (_, message_id) = seed_thread(&pool, None, "kunde@example.com").await;

        assert!(admin_repo::set_message_handled(&pool, message_id, true)
            .await
            .expect("handle"));
        assert!(message_state(&pool, message_id).await.1.is_some());

        assert!(admin_repo::set_message_handled(&pool, message_id, false)
            .await
            .expect("unhandle"));
        assert!(
            message_state(&pool, message_id).await.1.is_none(),
            "un-ticking must bring the reminder back, not leave a stale timestamp"
        );
    }

    #[tokio::test]
    async fn an_attachment_without_a_stored_name_falls_back_to_its_key() {
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();
        let (_, message_id) = seed_thread(&pool, None, "kunde@example.com").await;

        // Rows written before `attachment_names` existed have keys but no names.
        sqlx::query("UPDATE email_messages SET attachment_keys = $2 WHERE id = $1")
            .bind(message_id)
            .bind(vec!["emails/t/m/0.pdf".to_string()])
            .execute(&pool)
            .await
            .expect("set keys");

        let attachments = admin_repo::fetch_message_attachments(&pool, message_id)
            .await
            .expect("fetch attachments");

        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].1, "0.pdf");
    }

    #[tokio::test]
    async fn the_document_picker_lists_a_customers_kva_and_rechnung() {
        let state = crate::test_helpers::test_app_state().await;
        let pool = state.db.clone();

        let inquiry_id = crate::test_helpers::insert_test_quote(&pool).await;
        let (customer_id,): (Uuid,) =
            sqlx::query_as("SELECT customer_id FROM inquiries WHERE id = $1")
                .bind(inquiry_id)
                .fetch_one(&pool)
                .await
                .expect("customer of inquiry");

        sqlx::query("UPDATE customers SET last_name = 'Krause' WHERE id = $1")
            .bind(customer_id)
            .execute(&pool)
            .await
            .expect("set last name");

        let offer_id = crate::test_helpers::insert_test_offer(&pool, inquiry_id, "sent").await;
        sqlx::query("UPDATE offers SET offer_number = '2026-0131' WHERE id = $1")
            .bind(offer_id)
            .execute(&pool)
            .await
            .expect("set offer number");

        let invoice_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type, status, \
             pdf_s3_key, created_at) \
             VALUES ($1, $2, $3, 'full', 'ready', 'invoices/x.pdf', NOW())",
        )
        .bind(invoice_id)
        .bind(inquiry_id)
        // Unique per run: invoice_number is globally UNIQUE and this test shares the
        // database with every other one, so a time-derived prefix collides.
        .bind(format!("TEST-{invoice_id}"))
        .execute(&pool)
        .await
        .expect("insert invoice");

        // A second invoice with no PDF yet must not be offered — it is not sendable.
        sqlx::query(
            "INSERT INTO invoices (id, inquiry_id, invoice_number, invoice_type, status, created_at) \
             VALUES ($1, $2, $3, 'full', 'draft', NOW())",
        )
        .bind(Uuid::now_v7())
        .bind(inquiry_id)
        .bind(format!("TEST-nopdf-{invoice_id}"))
        .execute(&pool)
        .await
        .expect("insert pdf-less invoice");

        let docs = admin_repo::fetch_thread_documents(&pool, Some(customer_id), Some(inquiry_id))
            .await
            .expect("fetch documents");

        let kinds: Vec<&str> = docs.iter().map(|d| d.kind.as_str()).collect();
        assert_eq!(kinds.len(), 2, "expected exactly the two ready documents: {kinds:?}");
        assert!(kinds.contains(&"offer"));
        assert!(kinds.contains(&"invoice"));

        let offer = docs.iter().find(|d| d.kind == "offer").expect("offer listed");
        assert_eq!(offer.filename, "131-2026 Krause.pdf");
    }

    #[tokio::test]
    async fn a_thread_without_customer_or_inquiry_has_no_documents() {
        let state = crate::test_helpers::test_app_state().await;
        let docs = admin_repo::fetch_thread_documents(&state.db, None, None)
            .await
            .expect("fetch documents");
        assert!(docs.is_empty());
    }

    #[test]
    fn sanitize_html_drops_scripts_and_handlers() {
        let dirty = r#"<p onclick="steal()">Hallo <script>alert(1)</script><b>Welt</b></p>"#;
        let clean = sanitize_email_html(dirty);
        assert!(!clean.contains("script"), "script survived: {clean}");
        assert!(!clean.contains("onclick"), "handler survived: {clean}");
        assert!(clean.contains("Welt"), "content lost: {clean}");
    }

    #[test]
    fn sanitize_html_drops_tracking_pixels() {
        // A 1x1 remote image is how marketing mail reports that it was opened.
        let clean = sanitize_email_html(r#"<p>Hi</p><img src="https://track.example/p.gif">"#);
        assert!(!clean.contains("<img"), "image survived: {clean}");
        assert!(clean.contains("Hi"));
    }

    #[test]
    fn sanitize_filename_strips_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("plan.pdf"), "plan.pdf");
        assert_eq!(sanitize_filename("  "), "anhang");
        assert_eq!(sanitize_filename("a\"b.pdf"), "ab.pdf");
    }
}
