//! Bridge impl for `SettingsService`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use aust_core::services::{PricingConfig, ServiceError, SettingsService};
use aust_core::Config;

use crate::repositories::settings_repo;

pub struct SettingsServiceImpl {
    pool: PgPool,
    config: Arc<Config>,
}

impl SettingsServiceImpl {
    pub fn new(pool: PgPool, config: Arc<Config>) -> Self {
        Self { pool, config }
    }
}

#[async_trait]
impl SettingsService for SettingsServiceImpl {
    async fn get_pricing(&self) -> Result<PricingConfig, ServiceError> {
        let p = settings_repo::get_pricing(&self.pool, &self.config)
            .await
            .map_err(|e| ServiceError::Db(anyhow::anyhow!(e.to_string())))?;
        Ok(PricingConfig {
            base_rate_eur: p.rate_per_person_hour_cents as f64 / 100.0,
            saturday_surcharge_pct: 0.0, // surcharge is stored as cents, not pct — informational
            vat_rate_pct: 19.0,
            min_hours: 2.0,
        })
    }
}
