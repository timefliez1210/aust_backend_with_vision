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
use aust_core::tenant::{self, TenantId, AUST};

/// Run `tick` every `period`, once per tenant, each pass inside that tenant's
/// scope (`aust_core::tenant`) — so it reads and writes only that tenant's rows
/// and resolves that tenant's mailbox and bot.
fn every_tenant<F, Fut>(tenants: &Arc<Vec<TenantId>>, period: Duration, tick: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let tenants = tenants.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            for t in tenants.iter().copied() {
                tenant::scope(t, tick()).await;
            }
        }
    });
}

/// Whether the running tenant has a Telegram bot. Jobs whose only output is a
/// Telegram message skip tenants without one.
fn has_bot(cfg: &Config) -> bool {
    !cfg.telegram().bot_token.is_empty()
}

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

    // The KVA templates are laid out in Calibri metrics. Without a compatible face
    // LibreOffice re-wraps their text boxes and every generated KVA ships with a
    // broken terms page, so say so at boot instead of leaving it to be discovered
    // in a customer's PDF.
    match aust_offer_generator::check_template_fonts() {
        Ok(family) => tracing::info!("KVA rendering font: {family}"),
        Err(message) => tracing::error!("{message}"),
    }

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

    // `aust_backend tenant-create <slug> <name> <admin-email>`: onboard a company,
    // print its first admin's one-time password, exit. Restart the server after.
    let args: Vec<String> = std::env::args().collect();

    // `aust_backend superuser <email> on|off`: the only way to grant or revoke
    // platform superuser rights (the "Firmen" tab). Takes effect on next login.
    if args.get(1).map(String::as_str) == Some("superuser") {
        let email = args.get(2).cloned().unwrap_or_default();
        let on = match args.get(3).map(String::as_str) {
            Some("on") => true,
            Some("off") => false,
            _ => anyhow::bail!("Aufruf: aust_backend superuser <email> on|off"),
        };
        let mut tx = tenant::bypass(&db).await?;
        let changed = sqlx::query("UPDATE users SET is_superuser = $2 WHERE lower(email) = lower($1)")
            .bind(&email)
            .bind(on)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if changed == 0 {
            anyhow::bail!("Kein Benutzer mit E-Mail {email}");
        }
        println!("{email}: Plattform-Superuser {}", if on { "an" } else { "aus" });
        println!("Wirkt nach dem nächsten Login (der Reiter „Firmen“ erscheint dann).");
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("tenant-create") {
        let [slug, name, admin_email] = [2, 3, 4].map(|i| args.get(i).cloned().unwrap_or_default());
        let t = aust_api::services::onboarding::create_tenant(&db, &slug, &name, &admin_email)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!("Firma angelegt: {} ({slug})", t.id.0);
        println!("Admin: {}  Einmal-Passwort: {}", t.admin_email, t.admin_password);
        println!("Als Nächstes: tenants.domains setzen, [tenants.{slug}] (E-Mail, Telegram) konfigurieren,");
        println!("Vorlagen hochladen, Backend neu starten. Siehe docs/MULTI_TENANT.md.");
        return Ok(());
    }

    // Every tenant (company). Mailboxes and background jobs below run once per
    // tenant, each inside that tenant's scope. A new tenant needs a restart.
    let tenants = aust_core::tenant::all(&db).await?;
    aust_core::tenant::register_slugs(tenants.iter().cloned().collect());
    aust_core::tenant::register_domains(aust_core::tenant::all_domains(&db).await?.into_iter().collect());
    let tenant_ids: Arc<Vec<TenantId>> = Arc::new(tenants.iter().map(|(id, _)| *id).collect());
    tracing::info!(count = tenants.len(), "Tenants loaded");

    // A second company may only be served when Postgres enforces row-level
    // security, i.e. the app's role is neither superuser nor BYPASSRLS
    // (scripts/db-app-role.sql). Otherwise every unfiltered query would mix them.
    if tenants.len() > 1 {
        let (bypasses,): (bool,) = sqlx::query_as(
            "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&db)
        .await?;
        if bypasses {
            anyhow::bail!(
                "{} tenants, but the database role bypasses row-level security — \
                 run scripts/db-app-role.sql and connect as aust_app (docs/MULTI_TENANT.md)",
                tenants.len()
            );
        }
    }

    // Every other company's own document templates (Aust's are compiled in).
    {
        let mut tx = tenant::bypass(&db).await?;
        let rows: Vec<(TenantId, String, Vec<u8>)> =
            sqlx::query_as("SELECT tenant_id, kind, content FROM tenant_templates")
                .fetch_all(&mut *tx)
                .await?;
        for (tenant_id, kind, content) in rows {
            if let Some(kind) = aust_offer_generator::templates::TemplateKind::parse(&kind) {
                aust_offer_generator::templates::register(tenant_id, kind, content);
            }
        }
    }

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

    let cal_default_capacity = config.calendar.default_capacity;
    let cal_alternatives_count = config.calendar.alternatives_count;
    let cal_search_window_days = config.calendar.search_window_days;

    // Build assistant dependencies (LLM, tool registry, soul).
    // Soul is loaded from SOUL.md; missing file is non-fatal — falls back to a stub.
    let assistant_llm: std::sync::Arc<dyn aust_assistant::AssistantLlmProvider> = {
        let (base_url, api_key, main_model, cheap_model, vision_model) = config
            .llm
            .ollama
            .as_ref()
            .map(|o| {
                (
                    o.base_url.clone(),
                    o.api_key.clone(),
                    o.assistant_model.clone(),
                    o.assistant_cheap_model.clone(),
                    o.assistant_vision_model.clone(),
                )
            })
            .unwrap_or_else(|| {
                (
                    "http://localhost:11434".to_string(),
                    None,
                    aust_assistant::llm::DEFAULT_MAIN_MODEL.to_string(),
                    aust_assistant::llm::DEFAULT_CHEAP_MODEL.to_string(),
                    aust_assistant::llm::DEFAULT_VISION_MODEL.to_string(),
                )
            });
        tracing::info!(
            main_model = %main_model,
            cheap_model = %cheap_model,
            vision_model = %vision_model,
            "assistant LLM models"
        );
        std::sync::Arc::new(
            OllamaAssistantLlm::new(base_url, api_key)
                .with_models(main_model, cheap_model)
                .with_vision_model(vision_model),
        )
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

    // One mailbox per tenant: Aust's from the top-level config, every other
    // tenant's from `[tenants.<slug>]`. Each processor (IMAP poll, Telegram bot,
    // offer approvals, Josie) and its offer-event handler run inside the tenant's
    // scope, so everything they write lands in that tenant.
    for (tenant_id, slug) in &tenants {
        let tenant_id = *tenant_id;
        if tenant_id != AUST && !config.tenants.contains_key(slug) {
            tracing::info!(%slug, "No mailbox configured — skipping email processor");
            continue;
        }
        let cfg = state.config.clone();
        let db = state.db.clone();
        let storage = state.storage.clone();
        let llm = llm_for_email.clone();
        let handler_state = Arc::new(state.clone());
        let task_slug = slug.clone();
        tokio::spawn(tenant::scope(tenant_id, async move {
            let (offer_tx, offer_rx) = tokio::sync::mpsc::unbounded_channel();
            tenant::spawn(run_offer_event_handler(handler_state, offer_rx));
            let profile = match tenant::profile(&db).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(slug = %task_slug, "Email processor not started, profile not loaded: {e}");
                    return;
                }
            };
            let mut processor = EmailProcessor::new(
                cfg.email().clone(),
                cfg.telegram().clone(),
                llm,
                db,
                storage,
                cal_default_capacity,
                cal_alternatives_count,
                cal_search_window_days,
                profile,
            );
            processor.set_offer_channel(offer_tx);
            processor.run(cfg.email().poll_interval_secs).await;
        }));
        tracing::info!(tenant = %slug, "Email processor and offer event handler started");
    }

    // Periodic cleanup: mark estimations stuck in 'processing' for > 30 min as 'failed'.
    // This handles Modal container restarts or Rust panics that leave orphaned rows.
    {
        let db = state.db.clone();
        every_tenant(&tenant_ids, Duration::from_secs(300), move || {
            let db = db.clone();
            async move {
                if let Err(e) = sqlx::query(
                    "UPDATE volume_estimations SET status = 'failed' \
                     WHERE status = 'processing' AND created_at < NOW() - INTERVAL '30 minutes'",
                )
                .execute(&db)
                .await
                {
                    tracing::warn!("Stuck estimation cleanup failed: {e}");
                }
            }
        });
        tracing::info!("Stuck estimation cleanup task started");
    }

    // Periodic flash-contact reminders: notify the owner when the requested callback window begins.
    {
        let (db, cfg) = (state.db.clone(), state.config.clone());
        every_tenant(&tenant_ids, Duration::from_secs(120), move || {
            let (db, cfg) = (db.clone(), cfg.clone());
            async move {
                if !has_bot(&cfg) {
                    return;
                }
                if let Err(e) =
                    aust_api::services::flash_contact_service::run_reminder_check(&db, cfg.telegram()).await
                {
                    tracing::warn!("Flash contact reminder check failed: {e}");
                }
            }
        });
        tracing::info!("Flash contact reminder task started");
    }

    // Periodic vehicle reminders: ping the owner on TÜV/Ölwechsel/etc. as the due date
    // nears (21/14/7 days, then daily through the final week and while overdue).
    {
        let (db, cfg) = (state.db.clone(), state.config.clone());
        every_tenant(&tenant_ids, Duration::from_secs(60), move || {
            let (db, cfg) = (db.clone(), cfg.clone());
            async move {
                if !has_bot(&cfg) {
                    return;
                }
                if let Err(e) =
                    aust_api::services::vehicle_reminder_service::run_reminder_check(&db, cfg.telegram()).await
                {
                    tracing::warn!("Vehicle reminder check failed: {e}");
                }
            }
        });
        tracing::info!("Vehicle reminder task started");
    }

    // Periodic KVA follow-ups: ping the owner about Kostenvoranschläge that have gone
    // quiet past the threshold while the move date still lies ahead. Both
    // conditions must hold — a KVA whose Umzugsdatum has passed is dead.
    {
        let (db, cfg) = (state.db.clone(), state.config.clone());
        every_tenant(&tenant_ids, Duration::from_secs(60), move || {
            let (db, cfg) = (db.clone(), cfg.clone());
            async move {
                if !has_bot(&cfg) {
                    return;
                }
                if let Err(e) =
                    aust_api::services::kva_followup_service::run_followup_check(&db, cfg.telegram()).await
                {
                    tracing::warn!("KVA follow-up check failed: {e}");
                }
            }
        });
        tracing::info!("KVA follow-up task started");
    }

    // Storage-rental ("Lagerung") monthly billing: generate one invoice per active
    // contract on/after its anniversary day, awaiting Telegram/dashboard approval.
    // Hourly + the storage_invoices UNIQUE(contract, year, month) constraint =
    // exactly-once per calendar month, with catch-up if a tick is missed.
    {
        let (db, cfg, storage) = (state.db.clone(), state.config.clone(), state.storage.clone());
        every_tenant(&tenant_ids, Duration::from_secs(3600), move || {
            let (db, cfg, storage) = (db.clone(), cfg.clone(), storage.clone());
            async move {
                if let Err(e) =
                    aust_api::services::storage_billing_service::run_billing_tick(&db, &storage, &cfg).await
                {
                    tracing::warn!("Storage billing tick failed: {e}");
                }
            }
        });
        tracing::info!("Storage billing task started");
    }

    // ── Assistant event consumer ───────────────────────────────────────────────
    // One consumer per tenant with a bot: it sees only its tenant's domain events
    // (inquiry.created, offer.drafted, status.changed, …) and posts to its bot.
    for tenant_id in tenant_ids.iter().copied() {
        let cfg = state.config.clone();
        let db = state.db.clone();
        let services_arc = Arc::new(state.services.clone());
        tokio::spawn(tenant::scope(tenant_id, async move {
            if !has_bot(&cfg) {
                return;
            }
            let notifier: Arc<dyn TelegramNotifier> =
                Arc::new(TelegramNotifierImpl::new(cfg.telegram().bot_token.clone()));
            let consumer = AssistantEventConsumer::new(db, services_arc, notifier);
            let shutdown = tokio_util::sync::CancellationToken::new();
            consumer.run_forever(Duration::from_secs(5), shutdown).await;
        }));
    }
    tracing::info!("Assistant event consumers started (5 s poll)");

    // ── Pending-action expiry loop ─────────────────────────────────────────────
    // Marks timed-out pending_actions as 'expired' every 5 minutes.
    {
        let (db, cfg) = (state.db.clone(), state.config.clone());
        every_tenant(&tenant_ids, Duration::from_secs(300), move || {
            let (db, cfg) = (db.clone(), cfg.clone());
            async move {
                match aust_assistant::confirmation::expire_stale(&db).await {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!("Expired {n} stale pending_action(s)");
                        if !has_bot(&cfg) {
                            return;
                        }
                        // Notify the owner chat if any pending actions expired.
                        let owner_chat: Option<(i64,)> = sqlx::query_as(
                            "SELECT chat_id FROM telegram_chat_bindings WHERE role = 'owner' LIMIT 1",
                        )
                        .fetch_optional(&db)
                        .await
                        .ok()
                        .flatten();
                        if let Some((chat_id,)) = owner_chat {
                            let _ = TelegramNotifierImpl::new(cfg.telegram().bot_token.clone())
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
        let db = state.db.clone();
        let ids = tenant_ids.clone();
        tokio::spawn(async move {
            // Stagger the first run by 10 minutes so startup isn't noisy.
            tokio::time::sleep(Duration::from_secs(600)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(6 * 3600));
            loop {
                interval.tick().await;
                for t in ids.iter().copied() {
                    tenant::scope(t, aust_assistant::retention::run_retention_pass(&db)).await;
                }
            }
        });
        tracing::info!("Retention sweeper started (6 h interval)");
    }

    // ── Reminder tick ───────────────────────────────────────────────────────────
    // Every 60s: reconcile the unhandled-email nag and fire any due reminders
    // (set_reminder + the auto email reminders) back to Telegram.
    {
        let (db, cfg) = (state.db.clone(), state.config.clone());
        every_tenant(&tenant_ids, Duration::from_secs(60), move || {
            let (db, cfg) = (db.clone(), cfg.clone());
            async move {
                if !has_bot(&cfg) {
                    return;
                }
                let notifier = TelegramNotifierImpl::new(cfg.telegram().bot_token.clone());
                if let Err(e) = aust_assistant::hooks::reminders::run_reminder_tick(&db, &notifier).await {
                    tracing::warn!("Reminder tick failed: {e}");
                }
            }
        });
        tracing::info!("Reminder tick started (60 s interval)");
    }

    // ── Daily briefing tick ───────────────────────────────────────────────────
    // Every 60s: post the daily briefing to the owner chat at the fixed slots
    // (07:00 + 15:00 Europe/Berlin). Idempotent per (date, slot) via
    // agent_briefing_log, so this cadence just polls whether a slot is due.
    {
        let (db, cfg) = (state.db.clone(), state.config.clone());
        every_tenant(&tenant_ids, Duration::from_secs(60), move || {
            let (db, cfg) = (db.clone(), cfg.clone());
            async move {
                if !has_bot(&cfg) {
                    return;
                }
                let notifier = TelegramNotifierImpl::new(cfg.telegram().bot_token.clone());
                if let Err(e) = aust_assistant::hooks::briefing::run_briefing_tick(&db, &notifier).await {
                    tracing::warn!("Daily briefing tick failed: {e}");
                }
            }
        });
        tracing::info!("Daily briefing tick started (60 s interval, slots 07:00 + 15:00)");
    }

    // Create router and start server
    let app = create_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.server.port));
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    // with_connect_info so the rate limiter can see who actually opened the socket.
    // Without it there is no peer address at all and the limiter had to believe
    // X-Forwarded-For, which the caller writes.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
