/// Quick IMAP connectivity test.
/// Run with: cargo run -p aust-email-agent --example test_imap
use aust_core::config::EmailConfig;
use aust_email_agent::ImapClient;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .init();

    let config = EmailConfig {
        imap_host: std::env::var("AUST__EMAIL__IMAP_HOST")
            .unwrap_or_else(|_| "imap.example.com".to_string()),
        imap_port: std::env::var("AUST__EMAIL__IMAP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(993),
        smtp_host: std::env::var("AUST__EMAIL__SMTP_HOST")
            .unwrap_or_else(|_| "imap.example.com".to_string()),
        smtp_port: 587,
        username: std::env::var("AUST__EMAIL__USERNAME")
            .unwrap_or_else(|_| "umzug@example.com".to_string()),
        password: std::env::var("AUST__EMAIL__PASSWORD")
            .expect("Set AUST__EMAIL__PASSWORD env var"),
        poll_interval_secs: 60,
        from_address: "umzug@example.com".to_string(),
        from_name: "AUST Umzüge".to_string(),
    };

    let client = ImapClient::new(config);

    println!("=== Testing IMAP Connection ===\n");

    // Test 1: Connect and list mailboxes
    println!("1. Listing mailboxes...");
    match client.test_connection().await {
        Ok(mailboxes) => {
            println!("   Found {} mailboxes:", mailboxes.len());
            for mb in &mailboxes {
                println!("   - {mb}");
            }
        }
        Err(e) => {
            eprintln!("   FAILED: {e}");
            std::process::exit(1);
        }
    }

    println!();

    // Test 2: Fetch unread emails
    println!("2. Fetching unread emails...");
    match client.fetch_unread().await {
        Ok(emails) => {
            println!("   Found {} unread emails:", emails.len());
            for email in &emails {
                println!("   ---");
                println!("   From:    {}", email.from);
                println!("   Subject: {}", email.subject);
                println!("   Date:    {}", email.date);
                println!(
                    "   Body:    {}...",
                    email.body_text.chars().take(100).collect::<String>()
                );
                if !email.attachments.is_empty() {
                    println!("   Attachments: {}", email.attachments.len());
                    for att in &email.attachments {
                        println!(
                            "     - {} ({}, {} bytes)",
                            att.filename,
                            att.content_type,
                            att.data.len()
                        );
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("   FAILED: {e}");
            std::process::exit(1);
        }
    }

    println!("\n=== Done ===");
}
