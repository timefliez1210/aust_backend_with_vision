use serde::Deserialize;

/// Root application configuration, deserialized from `config/*.toml` and
/// environment variable overrides (`AUST__SECTION__KEY`).
///
/// **Caller**: `src/main.rs` at startup, then passed into every service constructor.
/// **Why**: Single source of truth for all service parameters so that deployment
/// environments (dev/prod) can differ purely through config without code changes.
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
    #[serde(default)]
    pub company: CompanyConfig,
}

/// HTTP server bind address and port.
///
/// **Why**: Separating host/port allows the server to bind to a specific
/// interface (e.g., `127.0.0.1` behind a reverse proxy vs `0.0.0.0` in dev).
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Network interface to bind to (e.g., `"0.0.0.0"` or `"127.0.0.1"`).
    pub host: String,
    /// TCP port to listen on (default: `8080`).
    pub port: u16,
}

/// PostgreSQL connection settings.
///
/// **Caller**: `sqlx::PgPool` is built from these values at startup.
#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    /// Full PostgreSQL connection URL, e.g. `postgres://user:pass@host/db`.
    pub url: String,
    /// Maximum number of pooled connections.
    pub max_connections: u32,
}

/// Object storage configuration for offer PDFs and estimation images.
///
/// **Caller**: `crates/storage` factory reads this to create either an
/// `S3Storage` or `LocalStorage` implementation.
#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    /// Backend selector: `"s3"` or `"local"`.
    pub provider: String,
    /// S3 bucket name (or the local directory name for `LocalStorage`).
    pub bucket: String,
    /// Custom S3-compatible endpoint URL (e.g., MinIO); `None` uses AWS default.
    pub endpoint: Option<String>,
    /// AWS region (e.g., `"eu-central-1"`); `None` for MinIO/local.
    pub region: Option<String>,
}

/// IMAP/SMTP settings for the email agent.
///
/// **Caller**: `EmailProcessor` in `crates/email-agent` uses this to poll
/// incoming mail (IMAP) and send responses (SMTP).
#[derive(Debug, Clone, Deserialize)]
pub struct EmailConfig {
    /// IMAP server hostname.
    pub imap_host: String,
    /// IMAP server port (typically `993` for TLS).
    pub imap_port: u16,
    /// SMTP server hostname.
    pub smtp_host: String,
    /// SMTP server port (typically `587` for STARTTLS or `465` for TLS).
    pub smtp_port: u16,
    /// Login username for both IMAP and SMTP.
    pub username: String,
    /// Login password for both IMAP and SMTP.
    pub password: String,
    /// How often (in seconds) the IMAP inbox is polled for new messages.
    pub poll_interval_secs: u64,
    /// SMTP "From" address shown to recipients.
    pub from_address: String,
    /// SMTP "From" display name shown to recipients.
    pub from_name: String,
}

/// LLM provider selection and per-provider credentials.
///
/// **Caller**: `crates/llm-providers::create_provider()` reads this to
/// instantiate the correct backend at startup.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    /// Which backend to use: `"claude"`, `"openai"`, or `"ollama"`.
    pub default_provider: String,
    /// Anthropic Claude credentials; required when `default_provider = "claude"`.
    pub claude: Option<ClaudeConfig>,
    /// OpenAI credentials; required when `default_provider = "openai"`.
    pub openai: Option<OpenAiConfig>,
    /// Ollama local server settings; required when `default_provider = "ollama"`.
    pub ollama: Option<OllamaConfig>,
}

/// Anthropic Claude API credentials.
///
/// **Why**: Claude is the primary LLM for German language generation and
/// vision-based furniture detection.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeConfig {
    /// Anthropic API key (`sk-ant-...`).
    pub api_key: String,
    /// Model identifier, e.g. `"claude-opus-4-6"`.
    pub model: String,
}

/// OpenAI API credentials (alternative LLM backend).
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiConfig {
    /// OpenAI API key (`sk-...`).
    pub api_key: String,
    /// Model identifier, e.g. `"gpt-4o"`.
    pub model: String,
}

/// Ollama local server settings (self-hosted, privacy-focused alternative).
#[derive(Debug, Clone, Deserialize)]
pub struct OllamaConfig {
    /// Base URL of the Ollama HTTP server, e.g. `"http://localhost:11434"`.
    pub base_url: String,
    /// Model name as registered in Ollama, e.g. `"llama3"`.
    pub model: String,
    /// Optional bearer token when Ollama is behind an authenticated proxy.
    pub api_key: Option<String>,
}

/// Geocoding / routing provider settings.
///
/// **Caller**: `crates/distance-calculator` uses this to call the
/// OpenRouteService API for address geocoding and route distance calculation.
#[derive(Debug, Clone, Deserialize)]
pub struct MapsConfig {
    /// Provider name (currently only `"openrouteservice"` is supported).
    pub provider: String,
    /// OpenRouteService API key, passed as `?api_key=` on every request.
    pub api_key: String,
}

/// Telegram Bot API settings for the human-in-the-loop approval workflow.
///
/// **Caller**: `EmailProcessor` in `crates/email-agent` sends generated offer
/// drafts to the admin chat for Alex to approve, edit, or deny.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    /// Telegram Bot API token (`123456:ABCDEF...`) obtained from BotFather.
    pub bot_token: String,
    /// Numeric chat ID of the admin (Alex); messages and approval buttons are
    /// sent only to this chat.
    pub admin_chat_id: i64,
}

/// JWT authentication settings for the admin REST API.
///
/// **Caller**: `crates/api` auth middleware validates access tokens against
/// these values.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    /// HMAC secret used to sign and verify JWT tokens.
    pub jwt_secret: String,
    /// Number of hours before an access token expires.
    pub jwt_expiry_hours: u64,
}

/// Calendar booking and capacity settings.
///
/// **Caller**: `CalendarService::new()` in `crates/calendar` receives these
/// values from the root `Config`.
#[derive(Debug, Clone, Deserialize)]
pub struct CalendarConfig {
    /// Maximum number of moving jobs that can be booked on a single day.
    /// Can be overridden per-date in the database.
    pub default_capacity: i32,
    /// How many alternative available dates to suggest when the requested date
    /// is fully booked.
    pub alternatives_count: usize,
    /// Maximum number of days to search forward/backward when looking for
    /// alternative dates.
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

/// External Python vision service configuration (Grounding DINO + SAM 2 + depth).
///
/// **Caller**: `crates/volume-estimator` and the `estimates/depth-sensor` +
/// `estimates/video` API routes forward image/video bytes to this service.
#[derive(Debug, Clone, Deserialize)]
pub struct VisionServiceConfig {
    /// Whether the vision service integration is active. When `false`, the
    /// API skips the vision service entirely and returns an error.
    pub enabled: bool,
    /// Base URL of the photo vision service (Modal deployment or localhost).
    pub base_url: String,
    /// Optional separate base URL for the video reconstruction endpoint.
    /// Falls back to `base_url` when `None`.
    #[serde(default)]
    pub video_base_url: Option<String>,
    /// HTTP request timeout in seconds for individual submit/poll requests.
    /// Submit should complete in <60s; poll responses are tiny (~1ms).
    pub timeout_secs: u64,
    /// Number of times to resubmit a job after a `failed` or `not_found` status.
    pub max_retries: u32,
    /// Seconds to wait between polling attempts for async jobs.
    /// Default: 60s (Modal containers stay warm for at least 60s of idle).
    pub poll_interval_secs: u64,
    /// Maximum number of poll attempts before declaring the job timed out.
    /// Default: 20 (20 × 60s = 20 min ceiling for photo; video may need more).
    pub max_polls: u32,
}

impl Default for VisionServiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://localhost:8090".to_string(),
            video_base_url: None,
            timeout_secs: 120,
            max_retries: 1,
            poll_interval_secs: 60,
            max_polls: 20,
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

/// Company-specific pricing and logistics constants.
///
/// **Caller**: `offer-generator` pricing engine uses `depot_address` as the
/// route start/end point and `fahrt_rate_per_km` to calculate the Anfahrt
/// (travel surcharge) line item.
#[derive(Debug, Clone, Deserialize)]
pub struct CompanyConfig {
    /// Full street address of the company depot, used as the origin when
    /// calculating the outbound travel distance for the Anfahrt line item.
    pub depot_address: String,
    /// Euro amount charged per kilometre for the Anfahrt/Abfahrt line item.
    /// For example, `1.5` means €1.50/km.
    pub fahrt_rate_per_km: f64,
}

impl Default for CompanyConfig {
    fn default() -> Self {
        Self {
            depot_address: "Borsigstr 6 31135 Hildesheim".to_string(),
            fahrt_rate_per_km: 1.0,
        }
    }
}
