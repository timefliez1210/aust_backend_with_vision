use aust_assistant::{AssistantLlmProvider, Soul, ToolRegistry};
use aust_core::events::EventEmitter;
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
    /// Service bridge for the assistant crate. Built once at startup.
    pub services: aust_core::services::ServiceBundle,
    /// Domain event emitter — writes non-fatal auxiliary events to `domain_events`.
    pub events: EventEmitter,
    /// LLM provider for the assistant driver (two-tier: main + cheap).
    pub assistant_llm: Arc<dyn AssistantLlmProvider>,
    /// Tool registry — all available tools pre-registered at startup.
    pub tool_registry: Arc<ToolRegistry>,
    /// Assistant soul — loaded from SOUL.md at startup (stub if file missing).
    pub soul: Arc<Soul>,
}

impl AppState {
    pub fn new(
        config: Config,
        db: PgPool,
        llm: Arc<dyn LlmProvider>,
        storage: Arc<dyn StorageProvider>,
        vision_service: Option<VisionServiceClient>,
        assistant_llm: Arc<dyn AssistantLlmProvider>,
        tool_registry: Arc<ToolRegistry>,
        soul: Arc<Soul>,
    ) -> Self {
        let config_arc = Arc::new(config);
        let services = crate::services::bridge::build_service_bundle(
            db.clone(),
            config_arc.clone(),
            storage.clone(),
        );
        let events = EventEmitter::new(db.clone());
        Self {
            config: config_arc,
            db,
            llm,
            storage,
            vision_service,
            vision_semaphore: Arc::new(Semaphore::new(1)),
            services,
            events,
            assistant_llm,
            tool_registry,
            soul,
        }
    }
}
