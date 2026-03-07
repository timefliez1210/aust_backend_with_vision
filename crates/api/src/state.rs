use aust_core::Config;
use aust_llm_providers::LlmProvider;
use aust_storage::StorageProvider;
use aust_volume_estimator::VisionServiceClient;
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub llm: Arc<dyn LlmProvider>,
    pub storage: Arc<dyn StorageProvider>,
    pub vision_service: Option<VisionServiceClient>,
    /// Semaphore that limits concurrent Modal vision calls to 1.
    /// All background workers acquire this before calling the GPU service,
    /// so jobs are serialized and the L4 never sees two pipelines at once.
    pub vision_semaphore: Arc<Semaphore>,
}

impl AppState {
    pub fn new(
        config: Config,
        db: PgPool,
        llm: Arc<dyn LlmProvider>,
        storage: Arc<dyn StorageProvider>,
        vision_service: Option<VisionServiceClient>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            db,
            llm,
            storage,
            vision_service,
            vision_semaphore: Arc::new(Semaphore::new(1)),
        }
    }
}
