use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub storage: StorageConfig,
    pub email: EmailConfig,
    pub llm: LlmConfig,
    pub maps: MapsConfig,
    pub telegram: TelegramConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    pub calendar: CalendarConfig,
    #[serde(default)]
    pub vision_service: VisionServiceConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    pub provider: String,
    pub bucket: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmailConfig {
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    pub poll_interval_secs: u64,
    pub from_address: String,
    pub from_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    pub default_provider: String,
    pub claude: Option<ClaudeConfig>,
    pub openai: Option<OpenAiConfig>,
    pub ollama: Option<OllamaConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeConfig {
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OllamaConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MapsConfig {
    pub provider: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub admin_chat_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub jwt_expiry_hours: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CalendarConfig {
    pub default_capacity: i32,
    pub alternatives_count: usize,
    pub search_window_days: i64,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            default_capacity: 1,
            alternatives_count: 3,
            search_window_days: 14,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct VisionServiceConfig {
    pub enabled: bool,
    pub base_url: String,
    #[serde(default)]
    pub video_base_url: Option<String>,
    pub timeout_secs: u64,
    pub max_retries: u32,
}

impl Default for VisionServiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://localhost:8090".to_string(),
            video_base_url: None,
            timeout_secs: 120,
            max_retries: 1,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
        }
    }
}
