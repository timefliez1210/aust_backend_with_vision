use aust_calendar::CalendarService;
use aust_core::Config;
use aust_llm_providers::LlmProvider;
use aust_storage::StorageProvider;
use aust_volume_estimator::VisionServiceClient;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: PgPool,
    pub llm: Arc<dyn LlmProvider>,
    pub storage: Arc<dyn StorageProvider>,
    pub calendar: Arc<CalendarService>,
    pub vision_service: Option<VisionServiceClient>,
}

impl AppState {
    pub fn new(
        config: Config,
        db: PgPool,
        llm: Arc<dyn LlmProvider>,
        storage: Arc<dyn StorageProvider>,
        calendar: Arc<CalendarService>,
        vision_service: Option<VisionServiceClient>,
    ) -> Self {
        Self {
            config: Arc::new(config),
            db,
            llm,
            storage,
            calendar,
            vision_service,
        }
    }
}
