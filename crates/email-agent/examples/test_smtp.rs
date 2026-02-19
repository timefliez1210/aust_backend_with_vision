/// Test SMTP connectivity.
/// Run with: cargo run -p aust-email-agent --example test_smtp
use aust_core::config::EmailConfig;
use aust_email_agent::SmtpClient;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .init();

    let config = EmailConfig {
        imap_host: std::env::var("AUST__EMAIL__IMAP_HOST").unwrap_or_default(),
        imap_port: 993,
        smtp_host: std::env::var("AUST__EMAIL__SMTP_HOST")
            .expect("Set AUST__EMAIL__SMTP_HOST"),
        smtp_port: std::env::var("AUST__EMAIL__SMTP_PORT")
            .unwrap_or_else(|_| "587".to_string())
            .parse()
            .unwrap(),
        username: std::env::var("AUST__EMAIL__USERNAME")
            .expect("Set AUST__EMAIL__USERNAME"),
        password: std::env::var("AUST__EMAIL__PASSWORD")
            .expect("Set AUST__EMAIL__PASSWORD"),
        from_address: std::env::var("AUST__EMAIL__FROM_ADDRESS")
            .expect("Set AUST__EMAIL__FROM_ADDRESS"),
        from_name: std::env::var("AUST__EMAIL__FROM_NAME")
            .unwrap_or_else(|_| "AUST Umzüge".to_string()),
        poll_interval_secs: 60,
    };

    let client = SmtpClient::new(config);

    println!("=== Testing SMTP connection ===\n");
    match client.test_connection().await {
        Ok(()) => println!("SMTP connection test PASSED!"),
        Err(e) => eprintln!("SMTP connection test FAILED: {e}"),
    }
}
