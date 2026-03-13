use anyhow::Result;
use aust_api::{create_pool, create_router, run_offer_event_handler, AppState};
use aust_core::Config;
use aust_email_agent::EmailProcessor;
use config::{ConfigBuilder, Environment, File};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
    tracing::info!("Configuration loaded");

    // Create database pool
    let db = create_pool(&config.database.url, config.database.max_connections).await?;
    tracing::info!("Database pool created");

    // Run migrations
    sqlx::migrate!("./migrations").run(&db).await?;
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
    let llm_for_email = llm.clone();
    let email_config = config.email.clone();
    let telegram_config = config.telegram.clone();
    let db_for_email = db.clone();
    let cal_default_capacity = config.calendar.default_capacity;
    let cal_alternatives_count = config.calendar.alternatives_count;
    let cal_search_window_days = config.calendar.search_window_days;

    // Create app state
    let state = AppState::new(config.clone(), db, llm, storage, vision_service);

    // Start email processor as background task
    let poll_interval = config.email.poll_interval_secs;
    tokio::spawn(async move {
        let mut processor = EmailProcessor::new(
            email_config,
            telegram_config,
            llm_for_email,
            db_for_email,
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

    // Create router and start server
    let app = create_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server.port));
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn load_config() -> Result<Config> {
    let run_mode = std::env::var("RUN_MODE").unwrap_or_else(|_| "development".into());

    let config = ConfigBuilder::<config::builder::DefaultState>::default()
        .add_source(File::with_name("config/default").required(false))
        .add_source(File::with_name(&format!("config/{}", run_mode)).required(false))
        .add_source(Environment::with_prefix("AUST").separator("__"))
        .build()?;

    Ok(config.try_deserialize()?)
}
