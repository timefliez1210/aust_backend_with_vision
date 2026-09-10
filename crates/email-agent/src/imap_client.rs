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

/// How many messages one poll pulls down.
///
/// Each is fetched whole, attachments included, into memory in the main backend
/// process. The poll interval is short, so a backlog drains over a few cycles instead of
/// arriving all at once.
const MAX_MESSAGES_PER_POLL: usize = 25;


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

        // Cap one poll's worth. Every message is pulled in full — bodies, photos, video
        // attachments — into a Vec held in the main backend process, so a backlog of
        // large mails could take the whole backend down with it. The rest stay UNSEEN
        // and come back on the next poll.
        let mut seqs: Vec<u32> = unseen.iter().copied().collect();
        seqs.sort_unstable();
        if seqs.len() > MAX_MESSAGES_PER_POLL {
            warn!(
                "Fetching {} of {} unread messages this cycle; the rest follow next poll",
                MAX_MESSAGES_PER_POLL,
                seqs.len()
            );
            seqs.truncate(MAX_MESSAGES_PER_POLL);
        }

        let seq_set: String = seqs
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");

        // Fetch full message data (RFC822) using PEEK to avoid marking as read
        let mut fetch_stream = session
            .fetch(&seq_set, "(UID BODY.PEEK[] FLAGS)")
            .await
            .map_err(|e| EmailError::Imap(format!("Fetch failed: {e}")))?;

        let mut emails = Vec::new();
        let mut unparseable: Vec<u32> = Vec::new();
        let parser = MessageParser::default();

        while let Some(result) = fetch_stream.next().await {
            match result {
                Ok(fetch) => {
                    let uid = fetch.uid;
                    if let Some(body) = fetch.body() {
                        match parser.parse(body) {
                            Some(message) => {
                                let mut parsed = parse_mail_message(&message);
                                parsed.uid = uid;
                                emails.push(parsed);
                            }
                            None => {
                                // Dropping it silently meant refetching the whole message,
                                // attachments included, on every poll forever. Nothing can
                                // be done with a message the parser rejects, so record it
                                // and let the caller flag it.
                                warn!("Failed to parse email message (seq {})", fetch.message);
                                if let Some(uid) = uid {
                                    unparseable.push(uid);
                                }
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

        // Flag anything the parser could not read, so the next poll does not fetch it
        // again. It is already in the database's hands as far as we can take it.
        for uid in &unparseable {
            if let Err(e) = store_seen_by_uid(&mut session, *uid).await {
                warn!("Could not flag unparseable message (uid {uid}): {e}");
            }
        }

        session.logout().await.ok();

        info!("Successfully fetched {} emails", emails.len());
        Ok(emails)
    }

    /// Mark a message as read (add \Seen flag) by its IMAP message ID header.
    /// Flag one message read by its UID.
    ///
    /// **Why**: `mark_as_read` searches by Message-ID, which IMAP matches as a
    /// *substring* and returns as an unordered set — an id that is a prefix of another
    /// could flag the wrong mail and lose it. A UID names exactly one message and is
    /// stable across the session, so the caller flags the message it actually read.
    pub async fn mark_uid_as_read(&self, uid: u32) -> Result<(), EmailError> {
        let mut session = self.connect().await?;
        session
            .select("INBOX")
            .await
            .map_err(|e| EmailError::Imap(format!("Failed to select INBOX: {e}")))?;
        let result = store_seen_by_uid(&mut session, uid).await;
        session.logout().await.ok();
        result
    }

    pub async fn mark_as_read(&self, message_id: &str) -> Result<(), EmailError> {
        let mut session = self.connect().await?;

        session
            .select("INBOX")
            .await
            .map_err(|e| EmailError::Imap(format!("Failed to select INBOX: {e}")))?;

        // Search for the message by Message-ID header
        let query = format!("HEADER Message-ID \"{message_id}\"");
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


/// Set the `Seen` flag on one message addressed by UID.
///
/// Shared by the poll loop (for messages the parser rejected) and `mark_uid_as_read`.
async fn store_seen_by_uid(session: &mut ImapSession, uid: u32) -> Result<(), EmailError> {
    let mut store_stream = session
        .uid_store(uid.to_string(), r"+FLAGS (\Seen)")
        .await
        .map_err(|e| EmailError::Imap(format!("UID store flags failed: {e}")))?;
    while store_stream.next().await.is_some() {}
    drop(store_stream);
    Ok(())
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

    // In-Reply-To / References carry the conversation's ancestry. mail-parser hands
    // these back as either a single Text or a TextList depending on how the sending
    // client formatted the header, so both shapes have to be flattened.
    let in_reply_to = header_ids(message, "In-Reply-To").into_iter().next();
    let references = header_ids(message, "References");

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
        uid: None,
        from,
        to,
        subject,
        body_text,
        body_html,
        message_id,
        in_reply_to,
        references,
        date,
        attachments,
    }
}

/// Flatten a Message-ID-bearing header into a list of bare ids (angle brackets stripped).
///
/// **Caller**: `parse_mail_message`
/// **Why**: `In-Reply-To` and `References` both hold Message-IDs, but mail-parser
/// models them as `Text` when there is one and `TextList` when there are several.
/// Callers want a `Vec<String>` either way. Brackets are stripped because that is how
/// `email_messages.message_id` is stored (mail-parser's `message_id()` returns the
/// bare form), so the two must agree for thread lookup to match.
fn header_ids(message: &mail_parser::Message, name: &str) -> Vec<String> {
    use mail_parser::HeaderValue;
    let strip = |s: &str| s.trim().trim_start_matches('<').trim_end_matches('>').to_string();
    match message.header(name) {
        Some(HeaderValue::Text(t)) => vec![strip(t)],
        Some(HeaderValue::TextList(list)) => list.iter().map(|t| strip(t)).collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> ParsedEmail {
        let message = mail_parser::MessageParser::default()
            .parse(raw.as_bytes())
            .expect("parse test message");
        parse_mail_message(&message)
    }

    const BODY: &str = "\r\nHallo,\r\n\r\nkurze Rückfrage.\r\n";

    #[test]
    fn a_reply_carries_its_parent_and_ancestry() {
        let raw = format!(
            "From: kunde@example.com\r\n\
             To: angebot@aust-umzuege.de\r\n\
             Subject: Re: Ihr Umzugsangebot\r\n\
             Message-ID: <c3@example.com>\r\n\
             In-Reply-To: <b2@aust-umzuege.de>\r\n\
             References: <a1@example.com> <b2@aust-umzuege.de>\r\n{BODY}"
        );
        let email = parse(&raw);

        // Brackets are stripped because `message_id()` stores the bare form; thread
        // lookup compares the two directly.
        assert_eq!(email.in_reply_to.as_deref(), Some("b2@aust-umzuege.de"));
        assert_eq!(
            email.references,
            vec!["a1@example.com".to_string(), "b2@aust-umzuege.de".to_string()]
        );
        assert_eq!(email.message_id, "c3@example.com");
    }

    #[test]
    fn a_single_reference_is_still_a_list() {
        // Clients that send exactly one id produce a Text header, not a TextList.
        let raw = format!(
            "From: kunde@example.com\r\n\
             Subject: Re: Angebot\r\n\
             Message-ID: <two@example.com>\r\n\
             References: <one@example.com>\r\n{BODY}"
        );
        let email = parse(&raw);
        assert_eq!(email.references, vec!["one@example.com".to_string()]);
        assert_eq!(email.in_reply_to, None);
    }

    #[test]
    fn a_fresh_mail_has_no_ancestry() {
        let raw = format!(
            "From: kunde@example.com\r\n\
             Subject: Anfrage\r\n\
             Message-ID: <solo@example.com>\r\n{BODY}"
        );
        let email = parse(&raw);
        assert_eq!(email.in_reply_to, None);
        assert!(email.references.is_empty());
    }
}
