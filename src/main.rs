use anyhow::Result;
use aust_api::{create_pool, create_router, run_offer_event_handler, AppState};
use aust_core::Config;
use aust_email_agent::EmailProcessor;
use config::{ConfigBuilder, Environment, File};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Assistant bootstrap imports.
use aust_assistant::events::{AssistantEventConsumer, TelegramNotifier};
use aust_assistant::{OllamaAssistantLlm, Soul, ToolRegistry};
use aust_api::services::assistant_bridge::TelegramNotifierImpl;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file (ignore if missing)
    let _ = dotenvy::dotenv();

    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "aust_backend=debug,aust_api=debug,aust_email_agent=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    let config = load_config()?;
    config.validate().map_err(|e| anyhow::anyhow!("Configuration error: {e}"))?;
    tracing::info!("Configuration loaded");

    // Create database pool
    let db = create_pool(&config.database.url, config.database.max_connections).await?;
    tracing::info!("Database pool created");

    // Run migrations. ignore_missing: 20260428000000_backfill_end_date.sql was
    // back-dated (it references inquiries.end_date, which only exists from
    // 20260601000000) and broke every fresh database. It was renamed to
    // 20260611000000; prod still records the old version in _sqlx_migrations,
    // which must not be treated as an error.
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator.run(&db).await?;
    tracing::info!("Migrations completed");

    // Create LLM provider
    let llm = aust_llm_providers::create_provider(&config.llm)?;
    tracing::info!("LLM provider initialized");

    // Create storage provider
    let storage = aust_storage::create_provider(&config.storage).await?;
    tracing::info!("Storage provider initialized");

    // Create vision service client (if enabled)
    let vision_service = if config.vision_service.enabled {
        match aust_volume_estimator::VisionServiceClient::new(
            &config.vision_service.base_url,
            config.vision_service.video_base_url.as_deref(),
            config.vision_service.ar_base_url.as_deref(),
            config.vision_service.timeout_secs,
            config.vision_service.max_retries,
        ) {
            Ok(client) => {
                tracing::info!(
                    "Vision service client initialized: {}",
                    config.vision_service.base_url
                );
                Some(client)
            }
            Err(e) => {
                tracing::warn!("Failed to create vision service client: {e}");
                None
            }
        }
    } else {
        tracing::info!("Vision service disabled");
        None
    };

    // Create offer event channel (email agent → orchestrator)
    let (offer_tx, offer_rx) = tokio::sync::mpsc::unbounded_channel();

    // Create email processor
    let email_config = config.email.clone();
    let telegram_config = config.telegram.clone();
    let db_for_email = db.clone();
    let cal_default_capacity = config.calendar.default_capacity;
    let cal_alternatives_count = config.calendar.alternatives_count;
    let cal_search_window_days = config.calendar.search_window_days;

    // Build assistant dependencies (LLM, tool registry, soul).
    // Soul is loaded from SOUL.md; missing file is non-fatal — falls back to a stub.
    let assistant_llm: std::sync::Arc<dyn aust_assistant::AssistantLlmProvider> = {
        let (base_url, api_key) = config
            .llm
            .ollama
            .as_ref()
            .map(|o| (o.base_url.clone(), o.api_key.clone()))
            .unwrap_or_else(|| ("http://localhost:11434".to_string(), None));
        std::sync::Arc::new(OllamaAssistantLlm::new(base_url, api_key))
    };
    // Email auto-replies are generated through Josie's LLM (same resilient
    // `/api/chat` path), not the generic provider — see EmailResponder.
    let llm_for_email = assistant_llm.clone();
    let tool_registry = std::sync::Arc::new(ToolRegistry::new());
    let soul: std::sync::Arc<Soul> = {
        let soul_path = std::path::Path::new("SOUL.md");
        match aust_assistant::soul::load(soul_path) {
            Ok(s) => {
                tracing::info!("SOUL.md loaded");
                std::sync::Arc::new(s)
            }
            Err(e) => {
                tracing::warn!("SOUL.md not found or invalid ({e}); using stub soul");
                std::sync::Arc::new(Soul {
                    persona: "Ich bin der AUST-Assistent.".to_string(),
                    hard_rules: String::new(),
                    domain_primer: String::new(),
                    tone: String::new(),
                    escalation: String::new(),
                })
            }
        }
    };

    let storage_for_email = storage.clone();

    // Create app state
    let state = AppState::new(
        config.clone(),
        db,
        llm,
        storage,
        vision_service,
        assistant_llm,
        tool_registry,
        soul,
    );

    // Start email processor as background task
    let poll_interval = config.email.poll_interval_secs;
    tokio::spawn(async move {
        let mut processor = EmailProcessor::new(
            email_config,
            telegram_config,
            llm_for_email,
            db_for_email,
            storage_for_email,
            cal_default_capacity,
            cal_alternatives_count,
            cal_search_window_days,
        );
        processor.set_offer_channel(offer_tx);
        processor.run(poll_interval).await;
    });
    tracing::info!("Email processor started");

    // Start offer event handler (receives events from email agent's Telegram poller)
    let offer_state = Arc::new(state.clone());
    tokio::spawn(async move {
        run_offer_event_handler(offer_state, offer_rx).await;
    });
    tracing::info!("Offer event handler started");

    // Periodic cleanup: mark estimations stuck in 'processing' for > 30 min as 'failed'.
    // This handles Modal container restarts or Rust panics that leave orphaned rows.
    let cleanup_db = state.db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // every 5 min
        loop {
            interval.tick().await;
            if let Err(e) = sqlx::query(
                "UPDATE volume_estimations SET status = 'failed' \
                 WHERE status = 'processing' AND created_at < NOW() - INTERVAL '30 minutes'"
            )
            .execute(&cleanup_db)
            .await
            {
                tracing::warn!("Stuck estimation cleanup failed: {e}");
            }
        }
    });
    tracing::info!("Stuck estimation cleanup task started");

    // Periodic flash-contact reminders: notify Alex when the requested callback window begins.
    let reminder_db = state.db.clone();
    let reminder_tg = state.config.telegram.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(120)); // every 2 min
        loop {
            interval.tick().await;
            if let Err(e) = aust_api::services::flash_contact_service::run_reminder_check(
                &reminder_db, &reminder_tg,
            ).await {
                tracing::warn!("Flash contact reminder check failed: {e}");
            }
        }
    });
    tracing::info!("Flash contact reminder task started");

    // Periodic vehicle reminders: ping Alex on TÜV/Ölwechsel/etc. as the due date
    // nears (21/14/7 days, then daily through the final week and while overdue).
    let vehicle_reminder_db = state.db.clone();
    let vehicle_reminder_tg = state.config.telegram.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60)); // every 1 min
        loop {
            interval.tick().await;
            if let Err(e) = aust_api::services::vehicle_reminder_service::run_reminder_check(
                &vehicle_reminder_db, &vehicle_reminder_tg,
            ).await {
                tracing::warn!("Vehicle reminder check failed: {e}");
            }
        }
    });
    tracing::info!("Vehicle reminder task started");

    // ── Assistant event consumer ───────────────────────────────────────────────
    // Build the TelegramNotifier (concrete reqwest-backed impl) and spawn the
    // AssistantEventConsumer that drives event handlers (inquiry.created,
    // offer.drafted, status.changed, etc.).
    {
        let notifier: Arc<dyn TelegramNotifier> = Arc::new(
            TelegramNotifierImpl::new(config.telegram.bot_token.clone()),
        );
        let services_arc = Arc::new(state.services.clone());
        let consumer = AssistantEventConsumer::new(
            state.db.clone(),
            services_arc,
            notifier,
        );
        let shutdown = tokio_util::sync::CancellationToken::new();
        tokio::spawn(consumer.run_forever(Duration::from_secs(5), shutdown));
        tracing::info!("Assistant event consumer started (5 s poll)");
    }

    // ── Pending-action expiry loop ─────────────────────────────────────────────
    // Marks timed-out pending_actions as 'expired' every 5 minutes.
    {
        let expiry_pool = state.db.clone();
        let expiry_tg_token = config.telegram.bot_token.clone();
        let expiry_notifier = TelegramNotifierImpl::new(expiry_tg_token);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                match aust_assistant::confirmation::expire_stale(&expiry_pool).await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!("Expired {n} stale pending_action(s)");
                        // Notify the owner chat if any pending actions expired.
                        let owner_chat: Option<(i64,)> = sqlx::query_as(
                            "SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1"
                        )
                        .fetch_optional(&expiry_pool)
                        .await
                        .ok()
                        .flatten();
                        if let Some((chat_id,)) = owner_chat {
                            let _ = expiry_notifier
                                .post(chat_id, format!("⏰ {n} ausstehende Aktion(en) sind abgelaufen. Bitte erneut versuchen."))
                                .await;
                        }
                    }
                    Err(e) => tracing::warn!("expire_stale failed: {e}"),
                }
            }
        });
        tracing::info!("Pending-action expiry loop started");
    }

    // ── Retention sweeper ─────────────────────────────────────────────────────
    // Runs every 6 hours, cleaning up stale rows across assistant tables.
    {
        let retention_pool = state.db.clone();
        tokio::spawn(async move {
            // Stagger the first run by 10 minutes so startup isn't noisy.
            tokio::time::sleep(Duration::from_secs(600)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
            loop {
                interval.tick().await;
                aust_assistant::retention::run_retention_pass(&retention_pool).await;
            }
        });
        tracing::info!("Retention sweeper started (6 h interval)");
    }

    // ── Reminder tick ───────────────────────────────────────────────────────────
    // Every 60s: reconcile the unhandled-email nag and fire any due reminders
    // (set_reminder + the auto email reminders) back to Telegram.
    {
        let reminder_pool = state.db.clone();
        let reminder_notifier: Arc<dyn TelegramNotifier> =
            Arc::new(TelegramNotifierImpl::new(config.telegram.bot_token.clone()));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Err(e) = aust_assistant::hooks::reminders::run_reminder_tick(
                    &reminder_pool,
                    reminder_notifier.as_ref(),
                )
                .await
                {
                    tracing::warn!("Reminder tick failed: {e}");
                }
            }
        });
        tracing::info!("Reminder tick started (60 s interval)");
    }

    // Create router and start server
    let app = create_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server.port));
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Server shut down cleanly");
    Ok(())
}

/// Waits for SIGTERM (systemd stop/restart) or Ctrl-C, then returns so axum can
/// drain in-flight requests before the process exits.
///
/// **Caller**: `main()` — passed to `axum::serve().with_graceful_shutdown()`.
/// **Why**: Without this, `systemctl restart` sends SIGTERM and the kernel kills the
///          process immediately. Any request in the middle of XLSX→PDF generation or
///          S3 upload is terminated, leaving orphaned S3 objects or a half-written DB row.
///          With graceful shutdown, axum stops accepting new connections and waits for
///          active handlers to finish before the process exits.
async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { tracing::info!("Received Ctrl-C, shutting down"); },
        _ = sigterm => { tracing::info!("Received SIGTERM, shutting down"); },
    }
}

fn load_config() -> Result<Config> {
    let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".into());

    let config = ConfigBuilder::<config::builder::DefaultState>::default()
        .add_source(File::with_name("config/default").required(false))
        .add_source(File::with_name(&format!("config/{run_mode}")).required(false))
        .add_source(Environment::with_prefix("AUST").separator("__"))
        .build()?;

    Ok(config.try_deserialize()?)
}
