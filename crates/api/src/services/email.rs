use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
use lettre::Message;

/// One attachment to hang on an outbound message.
pub struct OutboundAttachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Everything needed to build one outbound message.
///
/// **Why a struct**: the two builders below took eight positional `&str`s between
/// them and still could not express CC, BCC, more than one attachment, or a
/// threading header. Adding those as further parameters would have made the call
/// sites unreadable and easy to transpose.
pub struct OutboundEmail<'a> {
    pub from_address: &'a str,
    pub from_name: &'a str,
    pub to: &'a str,
    pub cc: &'a [String],
    pub bcc: &'a [String],
    pub subject: &'a str,
    pub body: &'a str,
    /// RFC `Message-ID` this mail answers. Set it and the customer's mail client
    /// files the reply into the existing conversation instead of opening a new
    /// one — the admin send path never did this, so every reply from the
    /// dashboard started a fresh thread on their side.
    pub in_reply_to: Option<&'a str>,
    pub attachments: Vec<OutboundAttachment>,
}

impl<'a> OutboundEmail<'a> {
    /// Minimal message: one recipient, no CC/BCC, no attachments, no threading.
    pub fn new(
        from_address: &'a str,
        from_name: &'a str,
        to: &'a str,
        subject: &'a str,
        body: &'a str,
    ) -> Self {
        Self {
            from_address,
            from_name,
            to,
            cc: &[],
            bcc: &[],
            subject,
            body,
            in_reply_to: None,
            attachments: Vec::new(),
        }
    }
}

/// Build an outbound message with any combination of CC/BCC, attachments and threading.
///
/// **Caller**: `admin_emails::send_draft_email`
/// **Why `Result<_, String>`**: a bad CC address is user input from the compose form,
/// not a programming error, and `lettre::error::Error` has no variant that can say
/// *which* address failed to parse.
pub fn build_message(mail: &OutboundEmail<'_>) -> Result<Message, String> {
    let from_mailbox: lettre::message::Mailbox =
        format!("{} <{}>", mail.from_name, mail.from_address)
            .parse()
            .map_err(|e| format!("Ungültige Absenderadresse: {e}"))?;

    let to_mailbox: lettre::message::Mailbox = mail
        .to
        .parse()
        .map_err(|e| format!("Ungültige Empfängeradresse '{}': {e}", mail.to))?;

    let mut builder = Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject(mail.subject);

    for addr in mail.cc.iter().filter(|a| !a.trim().is_empty()) {
        let mailbox: lettre::message::Mailbox = addr
            .parse()
            .map_err(|e| format!("Ungültige CC-Adresse '{addr}': {e}"))?;
        builder = builder.cc(mailbox);
    }
    for addr in mail.bcc.iter().filter(|a| !a.trim().is_empty()) {
        let mailbox: lettre::message::Mailbox = addr
            .parse()
            .map_err(|e| format!("Ungültige BCC-Adresse '{addr}': {e}"))?;
        builder = builder.bcc(mailbox);
    }

    if let Some(parent) = mail.in_reply_to.filter(|s| !s.trim().is_empty()) {
        builder = builder
            .in_reply_to(parent.to_string())
            .references(parent.to_string());
    }

    if mail.attachments.is_empty() {
        return builder
            .body(mail.body.to_string())
            .map_err(|e| format!("E-Mail-Aufbau fehlgeschlagen: {e}"));
    }

    let mut multipart = MultiPart::mixed().singlepart(SinglePart::plain(mail.body.to_string()));
    for att in &mail.attachments {
        let content_type = ContentType::parse(&att.content_type)
            .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
        multipart = multipart.singlepart(
            Attachment::new(att.filename.clone()).body(att.data.clone(), content_type),
        );
    }

    builder
        .multipart(multipart)
        .map_err(|e| format!("E-Mail-Aufbau fehlgeschlagen: {e}"))
}

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
// service fn — args are distinct email fields
#[allow(clippy::too_many_arguments)]
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

/// Send an email via SMTP.
///
/// `smtp_tls` selects the transport security: `"none"` uses a plaintext
/// connection (local dev/staging → Mailpit, which has no STARTTLS), anything
/// else uses STARTTLS (production default).
pub async fn send_email(
    smtp_host: &str,
    smtp_port: u16,
    smtp_tls: &str,
    username: &str,
    password: &str,
    message: Message,
) -> anyhow::Result<()> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

    // Plaintext mode targets Mailpit, which advertises no AUTH mechanisms —
    // attaching credentials would fail with "No compatible authentication
    // mechanism was found", so skip auth entirely there.
    let mailer = if smtp_tls == "none" {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
            .port(smtp_port)
            .build()
    } else {
        let creds = Credentials::new(username.to_string(), password.to_string());
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(smtp_host)
            .map_err(|e| anyhow::anyhow!("SMTP relay setup failed: {e}"))?
            .port(smtp_port)
            .credentials(creds)
            .build()
    };

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
