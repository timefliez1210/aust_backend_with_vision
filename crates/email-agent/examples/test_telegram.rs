/// Test the Telegram bot approval flow.
/// Run with: cargo run -p aust-email-agent --example test_telegram
use aust_email_agent::TelegramBot;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .init();

    let bot_token =
        std::env::var("AUST__TELEGRAM__BOT_TOKEN").expect("Set AUST__TELEGRAM__BOT_TOKEN");
    let admin_chat_id: i64 = std::env::var("AUST__TELEGRAM__ADMIN_CHAT_ID")
        .expect("Set AUST__TELEGRAM__ADMIN_CHAT_ID")
        .parse()
        .expect("ADMIN_CHAT_ID must be a number");

    let mut bot = TelegramBot::new(bot_token, admin_chat_id);

    // Send a test draft
    println!("=== Sending test draft for approval ===\n");
    let draft_id = "test-001";
    match bot
        .send_draft_for_approval(
            draft_id,
            "kunde@example.com",
            "Re: Ihre Anfrage bei AUST Umzüge",
            "Sehr geehrte Frau Müller,\n\n\
             vielen Dank für Ihre Anfrage bezüglich Ihres Umzugs.\n\n\
             Um Ihnen ein passendes Angebot erstellen zu können, \
             benötigen wir noch folgende Informationen:\n\n\
             1. Ihre vollständige Einzugsadresse\n\
             2. Das gewünschte Umzugsdatum\n\
             3. Eine grobe Aufstellung Ihrer Möbel und Gegenstände \
             (alternativ können Sie uns Fotos der Räumlichkeiten senden)\n\n\
             Mit freundlichen Grüßen,\n\
             Ihr AUST Umzüge Team",
        )
        .await
    {
        Ok(msg) => println!("Draft sent! Telegram message_id: {}\n", msg.message_id),
        Err(e) => {
            eprintln!("Failed to send draft: {e}");
            return;
        }
    }

    // Poll for the response
    println!("=== Waiting for approval decision (press buttons in Telegram)... ===\n");
    println!("(Will poll for 60 seconds)\n");

    for i in 0..30 {
        match bot.poll_approvals().await {
            Ok(responses) => {
                for resp in &responses {
                    println!("Got decision for draft '{}': {:?}", resp.draft_id, resp.decision);
                }
                if !responses.is_empty() {
                    println!("\nDone!");
                    return;
                }
            }
            Err(e) => eprintln!("Poll error: {e}"),
        }

        if i % 5 == 0 {
            println!("  ...still waiting ({i}/30)");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    println!("Timeout — no decision received.");
}
