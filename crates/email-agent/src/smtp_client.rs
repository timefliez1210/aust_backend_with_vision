use crate::EmailError;
use aust_core::config::EmailConfig;
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::{debug, info};
use uuid::Uuid;

/// What a successful send tells the caller.
pub struct SentMail {
    /// SMTP response line, for the log.
    pub status: String,
    /// The `Message-ID` this mail went out with, stored bare (no angle brackets) to
    /// match how inbound ids are stored, so a reply can be threaded by ancestry.
    pub message_id: String,
}


/// Wrap a bare Message-ID in the angle brackets RFC 5322 requires.
///
/// Inbound ids are stored bare (`imap_client::header_ids` strips the brackets so thread
/// lookup can match), but `In-Reply-To` and `References` are written as raw header text.
/// Emitting `In-Reply-To: abc@host` is not a valid msg-id, and Gmail and Outlook simply
/// do not thread on it — every reply opened a new conversation on the customer's side.
fn as_msg_id(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('<') && t.ends_with('>') {
        t.to_string()
    } else {
        format!("<{t}>")
    }
}

pub struct SmtpClient {
    config: EmailConfig,
}

impl SmtpClient {
    pub fn new(config: EmailConfig) -> Self {
        Self { config }
    }

    /// Send an email via SMTP.
    /// Returns a status string from the SMTP server.
    pub async fn send(
        &self,
        to: &str,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
    ) -> Result<SentMail, EmailError> {
        // Do not log `to` — it is customer PII (AGENTS.md). Subject is non-PII.
        debug!("Sending email, subject: {subject}");

        let from_mailbox: Mailbox = format!(
            "{} <{}>",
            self.config.from_name, self.config.from_address
        )
        .parse()
        .map_err(|e| EmailError::Smtp(format!("Invalid from address: {e}")))?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|e| EmailError::Smtp(format!("Invalid to address: {e}")))?;

        let mut builder = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject);

        // Thread the reply: set In-Reply-To + References to the parent message's
        // RFC Message-ID so the customer's mail client groups this into the existing
        // conversation instead of opening a new thread. Skipped when there is no
        // parent (e.g. a fresh outbound mail).
        if let Some(parent_id) = in_reply_to.filter(|s| !s.trim().is_empty()) {
            let msg_id = as_msg_id(parent_id);
            builder = builder
                .in_reply_to(msg_id.clone())
                .references(msg_id);
        }

        // Set our own Message-ID rather than letting lettre invent one, so the id can be
        // stored with the row. Without it, a customer replying to us carried an
        // In-Reply-To pointing at a message we had no record of, ancestry matching could
        // never fire, and threading fell back to "same customer within 30 days" — so a
        // reply after 30 days opened a new thread.
        let message_id = format!("<{}@{}>", Uuid::now_v7(), self.message_id_domain());
        let message = builder
            .message_id(Some(message_id.clone()))
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| EmailError::Smtp(format!("Failed to build message: {e}")))?;

        let mailer = self.build_transport()?;

        let response = mailer
            .send(message)
            .await
            .map_err(|e| EmailError::Smtp(format!("SMTP send failed: {e}")))?;

        let status = format!("{} {}", response.code(), response.first_line().unwrap_or("OK"));
        let _ = to;
        info!("Email sent: {status}");
        Ok(SentMail {
            status,
            message_id: message_id.trim_matches(['<', '>']).to_string(),
        })
    }

    /// The domain part of a generated Message-ID: our own sending domain.
    fn message_id_domain(&self) -> String {
        self.config
            .from_address
            .rsplit('@')
            .next()
            .unwrap_or("aust-umzuege.de")
            .to_string()
    }

    /// Test SMTP connectivity.
    pub async fn test_connection(&self) -> Result<(), EmailError> {
        let mailer = self.build_transport()?;

        mailer
            .test_connection()
            .await
            .map_err(|e| EmailError::Smtp(format!("SMTP test failed: {e}")))?;

        info!("SMTP connection test successful");
        Ok(())
    }

    fn build_transport(&self) -> Result<AsyncSmtpTransport<Tokio1Executor>, EmailError> {
        let creds = Credentials::new(
            self.config.username.clone(),
            self.config.password.clone(),
        );

        let transport =
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.config.smtp_host)
                .map_err(|e| EmailError::Smtp(format!("SMTP relay setup failed: {e}")))?
                .port(self.config.smtp_port)
                .credentials(creds)
                .build();

        Ok(transport)
    }
}
