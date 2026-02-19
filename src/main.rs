use anyhow::Result;
use aust_api::{create_pool, create_router, AppState};
use aust_core::Config;
use config::{ConfigBuilder, Environment, File};
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "aust_backend=debug,aust_api=debug,tower_http=debug".into()),
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

    // Create app state
    let state = AppState::new(config.clone(), db, llm, storage);

    // Create router
    let app = create_router(state);

    // Start server
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
