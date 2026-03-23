//! Email dispatch — send offer PDFs to customers via SMTP, and manage offer email threads.

use crate::repositories::{customer_repo, email_repo};
use crate::AppState;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Send the offer PDF to a customer via SMTP with a standard German cover letter.
///
/// **Caller**: `handle_offer_approval` (approval flow) and the admin API route
/// `POST /api/v1/offers/{id}/send` (manual re-send from the dashboard).
/// **Why**: Encapsulates SMTP send logic and the standard body text so that both callers
/// produce identical emails. The offer PDF is attached under the filename `Angebot-<id>.pdf`.
///
/// Uses `crate::services::email::{build_email_with_attachment, send_email}` internally.
///
/// # Parameters
/// - `state` — shared application state (email config: SMTP host/port, credentials, from address)
/// - `to` — customer email address
/// - `pdf_bytes` — raw PDF bytes to attach
/// - `offer_id` — used for the attachment filename and for logging
///
/// # Returns
/// `Ok(())` on successful SMTP delivery.
///
/// # Errors
/// Returns `Err(String)` if the email cannot be built (e.g. invalid address) or SMTP delivery fails.
pub async fn send_offer_email(
    state: &AppState,
    to: &str,
    pdf_bytes: &[u8],
    offer_id: Uuid,
) -> Result<(), String> {
    use crate::services::email::{build_email_with_attachment, send_email};

    let email_config = &state.config.email;

    let body_text = "Sehr geehrte Damen und Herren,\n\n\
        anbei erhalten Sie unser Angebot für Ihren Umzug.\n\n\
        Bei Fragen stehen wir Ihnen gerne zur Verfügung.\n\n\
        Mit freundlichen Grüßen,\n\
        Ihr Umzugsteam";

    let message = build_email_with_attachment(
        &email_config.from_address,
        &email_config.from_name,
        to,
        "Ihr Umzugsangebot",
        body_text,
        pdf_bytes,
        &format!("Angebot-{offer_id}.pdf"),
        "application/pdf",
    )
    .map_err(|e| format!("Failed to build email: {e}"))?;

    send_email(
        &email_config.smtp_host,
        email_config.smtp_port,
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())?;

    info!("Offer email sent to {to}");
    Ok(())
}

/// Send the offer PDF to a customer via SMTP with a caller-supplied subject and body.
///
/// **Caller**: The admin API route `POST /api/v1/offers/{id}/send` when the request body
/// includes a custom `subject` and/or `body` (editable email draft feature).
/// **Why**: Alex sometimes wants to personalise the cover letter before sending, e.g. to
/// reference a phone conversation or add a special note. This variant accepts arbitrary
/// subject and body strings instead of the standard template used by `send_offer_email`.
///
/// # Parameters
/// - `state` — shared application state (email config)
/// - `to` — customer email address
/// - `pdf_bytes` — raw PDF bytes to attach
/// - `offer_id` — used for the attachment filename and for logging
/// - `subject` — custom email subject line
/// - `body` — custom plain-text email body
///
/// # Returns
/// `Ok(())` on successful SMTP delivery.
///
/// # Errors
/// Returns `Err(String)` if the email cannot be built or SMTP delivery fails.
pub async fn send_offer_email_custom(
    state: &AppState,
    to: &str,
    pdf_bytes: &[u8],
    offer_id: Uuid,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    use crate::services::email::{build_email_with_attachment, send_email};

    let email_config = &state.config.email;

    let message = build_email_with_attachment(
        &email_config.from_address,
        &email_config.from_name,
        to,
        subject,
        body,
        pdf_bytes,
        &format!("Angebot-{offer_id}.pdf"),
        "application/pdf",
    )
    .map_err(|e| format!("Failed to build email: {e}"))?;

    send_email(
        &email_config.smtp_host,
        email_config.smtp_port,
        &email_config.username,
        &email_config.password,
        message,
    )
    .await
    .map_err(|e| e.to_string())?;

    info!("Offer email sent to {to} (custom)");
    Ok(())
}

/// Find an existing email thread for an inquiry, or create a new one linked to it.
///
/// **Caller**: `handle_offer_approval` — needs a thread to store the outbound offer email draft.
/// **Why**: The offer email draft must live in a thread so it appears in the admin email view
/// and can be sent via the frontend. This covers three cases: thread already linked by `inquiry_id`,
/// thread linked by `customer_id`, or no thread yet (creates one).
///
/// # Parameters
/// - `state` — shared AppState (DB pool, no SMTP needed here)
/// - `inquiry_id` — the inquiry whose offer was approved
/// - `customer_email` — customer email address (used to look up customer_id for new threads)
///
/// # Returns
/// The thread UUID to insert the draft into, or `None` if creation failed.
pub(crate) async fn find_or_create_offer_thread(
    state: &AppState,
    inquiry_id: Uuid,
    customer_email: &str,
) -> Option<Uuid> {
    // 1. Thread already directly linked to this inquiry
    if let Ok(Some(tid)) = email_repo::find_thread_by_inquiry(&state.db, inquiry_id).await {
        return Some(tid);
    }

    // 2. Thread linked by customer (not yet linked to this inquiry)
    if let Ok(Some(tid)) = email_repo::find_thread_by_inquiry_customer(&state.db, inquiry_id).await
    {
        // Link thread to inquiry while we're here
        let _ = email_repo::link_thread_to_inquiry(&state.db, tid, inquiry_id).await;
        return Some(tid);
    }

    // 3. No thread exists — create one (e.g. manually-created admin inquiry)
    let customer = customer_repo::fetch_by_email(&state.db, customer_email)
        .await
        .unwrap_or(None);

    let Some(customer) = customer else {
        warn!("Cannot create email thread: customer not found for {customer_email}");
        return None;
    };

    let thread_id = Uuid::now_v7();
    match email_repo::create_thread(
        &state.db,
        thread_id,
        customer.id,
        inquiry_id,
        "Ihr Umzugsangebot — AUST Umzüge",
    )
    .await
    {
        Ok(_) => Some(thread_id),
        Err(e) => {
            error!("Failed to create email thread for offer draft: {e}");
            None
        }
    }
}
