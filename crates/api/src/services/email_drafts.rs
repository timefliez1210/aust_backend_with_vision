//! Sending a mailbox draft — one path for the admin "Senden" button and Josie.
//!
//! `POST /admin/emails/messages/{id}/send` and Josie's `send_email_draft` tool both
//! call [`send_draft`]: the reply is threaded into the customer's conversation
//! (In-Reply-To), the thread's active KVA PDF and hand-picked attachments go
//! along, the offer/inquiry advance to "sent" only once SMTP accepted the mail,
//! and the thread counts as answered.

use sqlx::PgPool;
use uuid::Uuid;

use aust_core::events::EventEmitter;
use aust_core::Config;
use aust_storage::StorageProvider;

use crate::repositories::admin_repo;
use crate::ApiError;

/// Send the draft `id` and return the address it went to.
pub(crate) async fn send_draft(
    db: &PgPool,
    storage: &dyn StorageProvider,
    config: &Config,
    events: &EventEmitter,
    id: Uuid,
) -> Result<String, ApiError> {
    use crate::repositories::offer_repo;
    use crate::services::email::{build_message, send_email, OutboundAttachment, OutboundEmail};

    let draft = admin_repo::fetch_draft_for_send(db, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Entwurf nicht gefunden oder bereits gesendet".into()))?;

    // A thread can exist with no customer, no contact address and no inbound mail
    // (a compose draft whose recipient was cleared). Refusing here beats handing
    // lettre an empty string and reporting an opaque SMTP error.
    let to_address = draft.to_address.filter(|a| a.contains('@')).ok_or_else(|| {
        ApiError::BadRequest("Kein Empfänger hinterlegt — bitte Adresse ergänzen.".into())
    })?;

    let subject = match draft.subject {
        Some(s) => s,
        None => crate::services::email::offer_mail_subject(
            &crate::repositories::tenant_repo::profile(db).await?,
        ),
    };
    let body = draft.body_text.unwrap_or_default();

    let mut attachments: Vec<OutboundAttachment> = Vec::new();

    // The thread's active offer PDF, when there is one. Unchanged behaviour — it is
    // attached implicitly, which is why the thread view surfaces its filename.
    let offer_ids = match (&draft.pdf_storage_key, draft.offer_id, draft.inquiry_id) {
        (Some(key), Some(oid), Some(iid)) => {
            let pdf_bytes = storage.download(key).await.map_err(|e| match e {
                aust_storage::StorageError::NotFound(_) => {
                    ApiError::NotFound("Angebot-PDF nicht gefunden.".into())
                }
                _ => ApiError::Internal(format!("PDF-Download fehlgeschlagen: {e}")),
            })?;

            let filename = match offer_repo::fetch_offer_filename_parts(db, oid).await {
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
    for (key, filename) in admin_repo::fetch_message_attachments(db, id).await? {
        if Some(key.as_str()) == implicit_key {
            continue;
        }
        let data = storage.download(&key).await.map_err(|e| {
            ApiError::Internal(format!("Anhang '{filename}' konnte nicht geladen werden: {e}"))
        })?;
        let ext = filename.rsplit('.').next().unwrap_or("bin");
        attachments.push(OutboundAttachment {
            filename: filename.clone(),
            content_type: crate::routes::admin::mime_from_ext(ext).to_string(),
            data: data.to_vec(),
        });
    }

    let email_cfg = &config.email();
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
        admin_repo::mark_offer_sent(db, oid, now).await?;
        admin_repo::mark_inquiry_offer_sent(db, iid, now).await?;

        let emitter = events.clone();
        let payload = serde_json::json!({ "offer_id": oid, "inquiry_id": iid });
        let aggregate = format!("offer:{oid}");
        aust_core::tenant::spawn(async move {
            if let Err(e) = emitter.emit("offer.sent", &aggregate, payload).await {
                tracing::warn!("Failed to emit offer.sent event: {e}");
            }
        });
    }

    admin_repo::mark_message_sent(db, id, &to_address).await?;

    // Answering the thread is what "erledigt" means; the Telegram nag stops here
    // rather than waiting for someone to tick the inbound mail off by hand.
    if let Err(e) = admin_repo::mark_thread_handled(db, draft.thread_id).await {
        tracing::warn!("Failed to mark thread {} handled: {e}", draft.thread_id);
    }

    Ok(to_address)
}
