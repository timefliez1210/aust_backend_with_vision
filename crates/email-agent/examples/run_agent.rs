/// Run the full email agent processing loop.
/// This connects to IMAP, processes incoming emails, sends drafts
/// to Telegram for approval, and sends approved emails via SMTP.
///
/// Run with: cargo run -p aust-email-agent --example run_agent
use aust_core::config::{EmailConfig, TelegramConfig};
use aust_email_agent::EmailProcessor;
use aust_llm_providers::OllamaProvider;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,aust_email_agent=debug".to_string()),
        )
        .init();

    let email_config = EmailConfig {
        imap_host: env("AUST__EMAIL__IMAP_HOST"),
        imap_port: env("AUST__EMAIL__IMAP_PORT").parse().unwrap_or(993),
        smtp_host: env("AUST__EMAIL__SMTP_HOST"),
        smtp_port: env("AUST__EMAIL__SMTP_PORT").parse().unwrap_or(587),
        username: env("AUST__EMAIL__USERNAME"),
        password: env("AUST__EMAIL__PASSWORD"),
        from_address: env("AUST__EMAIL__FROM_ADDRESS"),
        from_name: std::env::var("AUST__EMAIL__FROM_NAME")
            .unwrap_or_else(|_| "AUST Umzüge".to_string()),
        poll_interval_secs: std::env::var("AUST__EMAIL__POLL_INTERVAL_SECS")
            .unwrap_or_else(|_| "60".to_string())
            .parse()
            .unwrap_or(60),
    };

    let telegram_config = TelegramConfig {
        bot_token: env("AUST__TELEGRAM__BOT_TOKEN"),
        admin_chat_id: env("AUST__TELEGRAM__ADMIN_CHAT_ID")
            .parse()
            .expect("ADMIN_CHAT_ID must be a number"),
    };

    let poll_interval = email_config.poll_interval_secs;

    // Use Ollama as the LLM provider (local, free)
    let ollama_url = std::env::var("AUST__LLM__OLLAMA__BASE_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let ollama_model = std::env::var("AUST__LLM__OLLAMA__MODEL")
        .unwrap_or_else(|_| "llama3.2-vision".to_string());

    let llm: Arc<dyn aust_llm_providers::LlmProvider> =
        Arc::new(OllamaProvider::new(ollama_url, ollama_model));

    println!("=== AUST Email Agent ===");
    println!("IMAP: {}:{}", email_config.imap_host, email_config.imap_port);
    println!("SMTP: {}:{}", email_config.smtp_host, email_config.smtp_port);
    println!("Poll interval: {}s", poll_interval);
    println!("LLM: Ollama ({})", std::env::var("AUST__LLM__OLLAMA__MODEL").unwrap_or_default());
    println!("========================\n");

    let mut processor = EmailProcessor::new(email_config, telegram_config, llm);
    processor.run(poll_interval).await;
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("Set {key} env var"))
}
