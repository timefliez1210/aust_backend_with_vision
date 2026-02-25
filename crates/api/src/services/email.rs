use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
use lettre::Message;

/// Build a plain text email message without attachments.
pub fn build_plain_email(
    from_address: &str,
    from_name: &str,
    to_address: &str,
    subject: &str,
    body: &str,
) -> Result<Message, lettre::error::Error> {
    let from_mailbox: lettre::message::Mailbox = format!("{from_name} <{from_address}>")
        .parse()
        .map_err(|_| lettre::error::Error::MissingFrom)?;

    let to_mailbox: lettre::message::Mailbox = to_address
        .parse()
        .map_err(|_| lettre::error::Error::MissingTo)?;

    Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject(subject)
        .body(body.to_string())
}

/// Build an email message with a PDF attachment.
pub fn build_email_with_attachment(
    from_address: &str,
    from_name: &str,
    to_address: &str,
    subject: &str,
    body: &str,
    attachment_data: &[u8],
    attachment_name: &str,
    attachment_content_type: &str,
) -> Result<Message, lettre::error::Error> {
    let from_mailbox: lettre::message::Mailbox = format!("{from_name} <{from_address}>")
        .parse()
        .map_err(|_| lettre::error::Error::MissingFrom)?;

    let to_mailbox: lettre::message::Mailbox = to_address
        .parse()
        .map_err(|_| lettre::error::Error::MissingTo)?;

    let content_type = ContentType::parse(attachment_content_type)
        .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());

    let attachment =
        Attachment::new(attachment_name.to_string()).body(attachment_data.to_vec(), content_type);

    Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject(subject)
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain(body.to_string()))
                .singlepart(attachment),
        )
}

/// Send an email via SMTP (STARTTLS).
pub async fn send_email(
    smtp_host: &str,
    smtp_port: u16,
    username: &str,
    password: &str,
    message: Message,
) -> anyhow::Result<()> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

    let creds = Credentials::new(username.to_string(), password.to_string());

    let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(smtp_host)
        .map_err(|e| anyhow::anyhow!("SMTP relay setup failed: {e}"))?
        .port(smtp_port)
        .credentials(creds)
        .build();

    mailer
        .send(message)
        .await
        .map_err(|e| anyhow::anyhow!("SMTP send failed: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plain_email_message() {
        let msg = build_plain_email(
            "sender@test.com",
            "Test Sender",
            "recipient@test.com",
            "Test Subject",
            "Test Body",
        )
        .unwrap();
        let formatted = msg.formatted();
        let formatted_str = String::from_utf8_lossy(&formatted);
        assert!(formatted_str.contains("Test Subject"));
        assert!(formatted_str.contains("sender@test.com"));
        assert!(formatted_str.contains("recipient@test.com"));
    }

    #[test]
    fn build_email_with_pdf_attachment() {
        let pdf = vec![0x25, 0x50, 0x44, 0x46]; // %PDF magic bytes
        let msg = build_email_with_attachment(
            "sender@test.com",
            "Test Sender",
            "recipient@test.com",
            "Angebot",
            "Ihr Angebot anbei",
            &pdf,
            "angebot.pdf",
            "application/pdf",
        )
        .unwrap();
        let formatted = msg.formatted();
        let formatted_str = String::from_utf8_lossy(&formatted);
        assert!(formatted_str.contains("angebot.pdf"));
        assert!(formatted_str.contains("Angebot"));
    }

    #[test]
    fn build_plain_email_handles_german_characters() {
        let msg = build_plain_email(
            "umzug@aust-umzuege.de",
            "AUST Umzuege",
            "kunde@example.com",
            "Ihr Umzugsangebot",
            "Sehr geehrter Herr Mueller, anbei Ihr Angebot fuer den Umzug.",
        )
        .unwrap();
        // Should not panic — characters in subject + body
        let formatted = msg.formatted();
        assert!(!formatted.is_empty());
    }
}
