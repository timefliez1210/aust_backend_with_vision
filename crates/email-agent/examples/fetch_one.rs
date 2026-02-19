/// Fetch the most recent email (read or unread) and print it.
/// Run with: cargo run -p aust-email-agent --example fetch_one
use aust_core::config::EmailConfig;
use futures::StreamExt;
use mail_parser::{MessageParser, MimeHeaders};
use tokio::net::TcpStream;
use tokio_native_tls::native_tls;
use tokio_native_tls::TlsConnector;
use tokio_util::compat::TokioAsyncReadCompatExt;

#[tokio::main]
async fn main() {
    let host = env("AUST__EMAIL__IMAP_HOST");
    let port: u16 = env("AUST__EMAIL__IMAP_PORT").parse().unwrap_or(993);
    let username = env("AUST__EMAIL__USERNAME");
    let password = env("AUST__EMAIL__PASSWORD");

    // Connect
    let addr = format!("{host}:{port}");
    let tcp = TcpStream::connect(&addr).await.expect("TCP connect");
    let tls = native_tls::TlsConnector::new().expect("TLS");
    let tls = TlsConnector::from(tls);
    let stream = tls.connect(&host, tcp).await.expect("TLS handshake");
    let client = async_imap::Client::new(stream.compat());
    let mut session = client.login(&username, &password).await.map_err(|(e, _)| e).expect("Login");

    let mailbox = session.select("INBOX").await.expect("Select INBOX");
    let total = mailbox.exists;
    eprintln!("INBOX has {total} messages total");

    if total == 0 {
        println!("No emails in inbox.");
        return;
    }

    // Fetch the last message
    let seq = total.to_string();
    let mut fetch = session.fetch(&seq, "(BODY.PEEK[] FLAGS)").await.expect("Fetch");

    let parser = MessageParser::default();
    while let Some(Ok(msg)) = fetch.next().await {
        if let Some(body) = msg.body() {
            if let Some(parsed) = parser.parse(body) {
                let from = parsed.from().and_then(|a| a.first()).map(|a| {
                    a.address().unwrap_or_default().to_string()
                }).unwrap_or_default();
                let subject = parsed.subject().unwrap_or("(no subject)");
                let body_text = parsed.body_text(0).unwrap_or_default();

                println!("FROM: {from}");
                println!("SUBJECT: {subject}");
                println!("---BODY---");
                println!("{body_text}");
                println!("---END---");
            }
        }
    }

    drop(fetch);
    session.logout().await.ok();
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("Set {key}"))
}
