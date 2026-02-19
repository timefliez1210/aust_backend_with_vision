use crate::EmailError;
use aust_core::config::EmailConfig;
use aust_core::models::{EmailAttachment, ParsedEmail};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use mail_parser::{MessageParser, MimeHeaders};
use tokio::net::TcpStream;
use tokio_native_tls::native_tls;
use tokio_native_tls::TlsConnector;
use tokio_util::compat::TokioAsyncReadCompatExt;
use tracing::{debug, error, info, warn};

type ImapSession =
    async_imap::Session<tokio_util::compat::Compat<tokio_native_tls::TlsStream<TcpStream>>>;

pub struct ImapClient {
    config: EmailConfig,
}

impl ImapClient {
    pub fn new(config: EmailConfig) -> Self {
        Self { config }
    }

    async fn connect(&self) -> Result<ImapSession, EmailError> {
        let addr = format!("{}:{}", self.config.imap_host, self.config.imap_port);
        debug!("Connecting to IMAP server at {addr}");

        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| EmailError::Imap(format!("TCP connect failed: {e}")))?;

        let tls_connector = native_tls::TlsConnector::new()
            .map_err(|e| EmailError::Imap(format!("TLS connector creation failed: {e}")))?;
        let tls_connector = TlsConnector::from(tls_connector);

        let tls_stream = tls_connector
            .connect(&self.config.imap_host, tcp)
            .await
            .map_err(|e| EmailError::Imap(format!("TLS handshake failed: {e}")))?;

        let compat_stream = tls_stream.compat();

        let client = async_imap::Client::new(compat_stream);
        debug!("IMAP client created, logging in as {}", self.config.username);

        let session = client
            .login(&self.config.username, &self.config.password)
            .await
            .map_err(|(e, _client)| EmailError::Imap(format!("Login failed: {e}")))?;

        info!("Successfully logged in to IMAP as {}", self.config.username);
        Ok(session)
    }

    /// Fetch all unread (UNSEEN) emails from the INBOX.
    pub async fn fetch_unread(&self) -> Result<Vec<ParsedEmail>, EmailError> {
        let mut session = self.connect().await?;

        let mailbox = session
            .select("INBOX")
            .await
            .map_err(|e| EmailError::Imap(format!("Failed to select INBOX: {e}")))?;

        info!(
            "INBOX selected: {} total messages, {} unseen",
            mailbox.exists,
            mailbox.unseen.unwrap_or(0)
        );

        // Search for unseen messages
        let unseen = session
            .search("UNSEEN")
            .await
            .map_err(|e| EmailError::Imap(format!("Search UNSEEN failed: {e}")))?;

        if unseen.is_empty() {
            debug!("No unread messages found");
            session.logout().await.ok();
            return Ok(vec![]);
        }

        info!("Found {} unread messages", unseen.len());

        // Build sequence set from message IDs
        let seq_set: String = unseen
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");

        // Fetch full message data (RFC822) using PEEK to avoid marking as read
        let mut fetch_stream = session
            .fetch(&seq_set, "(BODY.PEEK[] FLAGS)")
            .await
            .map_err(|e| EmailError::Imap(format!("Fetch failed: {e}")))?;

        let mut emails = Vec::new();
        let parser = MessageParser::default();

        while let Some(result) = fetch_stream.next().await {
            match result {
                Ok(fetch) => {
                    if let Some(body) = fetch.body() {
                        match parser.parse(body) {
                            Some(message) => {
                                let parsed = parse_mail_message(&message);
                                emails.push(parsed);
                            }
                            None => {
                                warn!("Failed to parse email message (seq {})", fetch.message);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Error fetching message: {e}");
                }
            }
        }

        drop(fetch_stream);
        session.logout().await.ok();

        info!("Successfully fetched {} emails", emails.len());
        Ok(emails)
    }

    /// Mark a message as read (add \Seen flag) by its IMAP message ID header.
    pub async fn mark_as_read(&self, message_id: &str) -> Result<(), EmailError> {
        let mut session = self.connect().await?;

        session
            .select("INBOX")
            .await
            .map_err(|e| EmailError::Imap(format!("Failed to select INBOX: {e}")))?;

        // Search for the message by Message-ID header
        let query = format!("HEADER Message-ID \"{}\"", message_id);
        let results = session
            .search(&query)
            .await
            .map_err(|e| EmailError::Imap(format!("Search by Message-ID failed: {e}")))?;

        if let Some(&seq) = results.iter().next() {
            let mut store_stream = session
                .store(seq.to_string(), "+FLAGS (\\Seen)")
                .await
                .map_err(|e| EmailError::Imap(format!("Store flags failed: {e}")))?;

            // Consume the stream to apply the change
            while store_stream.next().await.is_some() {}
            drop(store_stream);

            debug!("Marked message {message_id} as read");
        } else {
            warn!("Message with ID {message_id} not found for marking as read");
        }

        session.logout().await.ok();
        Ok(())
    }

    /// Test connectivity — connect, login, list mailboxes, disconnect.
    pub async fn test_connection(&self) -> Result<Vec<String>, EmailError> {
        let mut session = self.connect().await?;

        let mut list_stream = session
            .list(Some(""), Some("*"))
            .await
            .map_err(|e| EmailError::Imap(format!("List mailboxes failed: {e}")))?;

        let mut mailboxes = Vec::new();
        while let Some(result) = list_stream.next().await {
            if let Ok(name) = result {
                mailboxes.push(name.name().to_string());
            }
        }

        drop(list_stream);
        session.logout().await.ok();

        Ok(mailboxes)
    }
}

fn parse_mail_message(message: &mail_parser::Message) -> ParsedEmail {
    let from = message
        .from()
        .and_then(|addrs| addrs.first())
        .map(|addr| {
            addr.address()
                .map(|a| a.to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let to = message
        .to()
        .and_then(|addrs| addrs.first())
        .map(|addr| {
            addr.address()
                .map(|a| a.to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let subject = message.subject().unwrap_or("").to_string();

    let body_text = message
        .body_text(0)
        .map(|t| t.to_string())
        .unwrap_or_default();

    let body_html = message.body_html(0).map(|h| h.to_string());

    let message_id = message.message_id().unwrap_or("").to_string();

    let date = message
        .date()
        .and_then(|d| DateTime::from_timestamp(d.to_timestamp(), 0))
        .unwrap_or_else(Utc::now);

    let mut attachments = Vec::new();
    for part in message.attachments() {
        let filename = part
            .attachment_name()
            .unwrap_or("unnamed")
            .to_string();

        let content_type = part
            .content_type()
            .map(|ct: &mail_parser::ContentType| {
                let mut s = ct.ctype().to_string();
                if let Some(subtype) = ct.subtype() {
                    s.push('/');
                    s.push_str(subtype);
                }
                s
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let data = part.contents().to_vec();

        attachments.push(EmailAttachment {
            filename,
            content_type,
            data,
        });
    }

    ParsedEmail {
        from,
        to,
        subject,
        body_text,
        body_html,
        message_id,
        date,
        attachments,
    }
}
