use anyhow::Result;
use aust_api::{create_pool, create_router, run_offer_event_handler, AppState};
use aust_calendar::CalendarService;
use aust_core::Config;
use aust_email_agent::EmailProcessor;
use config::{ConfigBuilder, Environment, File};
use std::net::SocketAddr;
use std::sync::Arc;
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
    tracing::info!("LLM provider initialized: {}", llm.name());

    // Create storage provider
    let storage = aust_storage::create_provider(&config.storage).await?;
    tracing::info!("Storage provider initialized");

    // Create calendar service
    let calendar = Arc::new(CalendarService::new(
        db.clone(),
        config.calendar.default_capacity,
        config.calendar.alternatives_count,
        config.calendar.search_window_days,
    ));
    tracing::info!("Calendar service initialized (default capacity: {})", config.calendar.default_capacity);

    // Create vision service client (if enabled)
    let vision_service = if config.vision_service.enabled {
        match aust_volume_estimator::VisionServiceClient::new(
            &config.vision_service.base_url,
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

    // Create email processor (needs LLM + calendar clones before they move into AppState)
    let llm_for_email = llm.clone();
    let calendar_for_email = calendar.clone();
    let email_config = config.email.clone();
    let telegram_config = config.telegram.clone();

    // Create app state
    let state = AppState::new(config.clone(), db, llm, storage, calendar, vision_service);

    // Start email processor as background task
    let poll_interval = config.email.poll_interval_secs;
    tokio::spawn(async move {
        let mut processor = EmailProcessor::new(
            email_config,
            telegram_config,
            llm_for_email,
            calendar_for_email,
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
