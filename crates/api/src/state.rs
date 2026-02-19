use aust_core::Config;
use aust_llm_providers::LlmProvider;
use aust_storage::StorageProvider;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub llm: Arc<dyn LlmProvider>,
    pub storage: Arc<dyn StorageProvider>,
}

impl AppState {
    pub fn new(
        config: Config,
        db: PgPool,
        llm: Arc<dyn LlmProvider>,
        storage: Arc<dyn StorageProvider>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            db,
            llm,
            storage,
        }
    }
}
