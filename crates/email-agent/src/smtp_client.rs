use crate::EmailError;
use aust_core::config::EmailConfig;
use lettre::message::header::ContentType;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::{debug, info};

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
        _in_reply_to: Option<&str>,
    ) -> Result<String, EmailError> {
        debug!("Sending email to {to}, subject: {subject}");

        let from_mailbox: Mailbox = format!(
            "{} <{}>",
            self.config.from_name, self.config.from_address
        )
        .parse()
        .map_err(|e| EmailError::Smtp(format!("Invalid from address: {e}")))?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|e| EmailError::Smtp(format!("Invalid to address: {e}")))?;

        let message = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| EmailError::Smtp(format!("Failed to build message: {e}")))?;

        let mailer = self.build_transport()?;

        let response = mailer
            .send(message)
            .await
            .map_err(|e| EmailError::Smtp(format!("SMTP send failed: {e}")))?;

        let status = format!("{} {}", response.code(), response.first_line().unwrap_or("OK"));
        info!("Email sent to {to}: {status}");
        Ok(status)
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
