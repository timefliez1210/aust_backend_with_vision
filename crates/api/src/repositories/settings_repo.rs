//! Repository for the `settings` table — runtime-editable configuration.
//!
//! Handles the standard pricing values (so they can be changed without a
//! redeploy) and the "next number" controls for the invoice/KVA sequences.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use aust_core::config::Config;

use crate::ApiError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Standard pricing values. Mirrors the pricing-relevant fields of
/// `CompanyConfig`; each field falls back to the config/env default when the
/// corresponding `settings` row is absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PricingSettings {
    pub rate_per_person_hour_cents: i64,
    pub saturday_surcharge_cents: i64,
    pub fahrt_rate_per_km: f64,
    pub assembly_price: f64,
    pub parking_ban_price: f64,
    pub packing_price: f64,
    pub transporter_price: f64,
}

impl PricingSettings {
    fn from_config(config: &Config) -> Self {
        Self {
            rate_per_person_hour_cents: config.company.rate_per_person_hour_cents,
            saturday_surcharge_cents: config.company.saturday_surcharge_cents,
            fahrt_rate_per_km: config.company.fahrt_rate_per_km,
            assembly_price: config.company.assembly_price,
            parking_ban_price: config.company.parking_ban_price,
            packing_price: config.company.packing_price,
            transporter_price: config.company.transporter_price,
        }
    }
}

/// Current "next number" for the invoice and KVA (offer) sequences.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct NextNumbers {
    pub next_invoice_number: i64,
    pub next_offer_number: i64,
}

// Pricing keys stored in the `settings` table.
const KEY_RATE_PER_PERSON_HOUR_CENTS: &str = "rate_per_person_hour_cents";
const KEY_SATURDAY_SURCHARGE_CENTS: &str = "saturday_surcharge_cents";
const KEY_FAHRT_RATE_PER_KM: &str = "fahrt_rate_per_km";
const KEY_ASSEMBLY_PRICE: &str = "assembly_price";
const KEY_PARKING_BAN_PRICE: &str = "parking_ban_price";
const KEY_PACKING_PRICE: &str = "packing_price";
const KEY_TRANSPORTER_PRICE: &str = "transporter_price";

// ---------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------

/// Load the effective pricing settings: DB rows override the config defaults
/// on a per-key basis.
///
/// **Caller**: `offer_builder::build_offer_with_overrides` and the admin
/// settings route.
pub(crate) async fn get_pricing(
    db: &PgPool,
    config: &Config,
) -> Result<PricingSettings, ApiError> {
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT key, value FROM settings")
            .fetch_all(db)
            .await?;
    let map: HashMap<String, serde_json::Value> = rows.into_iter().collect();

    let mut p = PricingSettings::from_config(config);
    if let Some(v) = map.get(KEY_RATE_PER_PERSON_HOUR_CENTS).and_then(|v| v.as_i64()) {
        p.rate_per_person_hour_cents = v;
    }
    if let Some(v) = map.get(KEY_SATURDAY_SURCHARGE_CENTS).and_then(|v| v.as_i64()) {
        p.saturday_surcharge_cents = v;
    }
    if let Some(v) = map.get(KEY_FAHRT_RATE_PER_KM).and_then(|v| v.as_f64()) {
        p.fahrt_rate_per_km = v;
    }
    if let Some(v) = map.get(KEY_ASSEMBLY_PRICE).and_then(|v| v.as_f64()) {
        p.assembly_price = v;
    }
    if let Some(v) = map.get(KEY_PARKING_BAN_PRICE).and_then(|v| v.as_f64()) {
        p.parking_ban_price = v;
    }
    if let Some(v) = map.get(KEY_PACKING_PRICE).and_then(|v| v.as_f64()) {
        p.packing_price = v;
    }
    if let Some(v) = map.get(KEY_TRANSPORTER_PRICE).and_then(|v| v.as_f64()) {
        p.transporter_price = v;
    }
    Ok(p)
}

/// Persist all pricing values, overwriting any existing rows.
pub(crate) async fn upsert_pricing(
    db: &PgPool,
    p: &PricingSettings,
) -> Result<(), ApiError> {
    let entries: [(&str, serde_json::Value); 7] = [
        (KEY_RATE_PER_PERSON_HOUR_CENTS, p.rate_per_person_hour_cents.into()),
        (KEY_SATURDAY_SURCHARGE_CENTS, p.saturday_surcharge_cents.into()),
        (KEY_FAHRT_RATE_PER_KM, serde_json::json!(p.fahrt_rate_per_km)),
        (KEY_ASSEMBLY_PRICE, serde_json::json!(p.assembly_price)),
        (KEY_PARKING_BAN_PRICE, serde_json::json!(p.parking_ban_price)),
        (KEY_PACKING_PRICE, serde_json::json!(p.packing_price)),
        (KEY_TRANSPORTER_PRICE, serde_json::json!(p.transporter_price)),
    ];
    for (key, value) in entries {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at)
             VALUES ($1, $2, NOW())
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
        )
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Number sequences (invoice / KVA)
// ---------------------------------------------------------------------------

/// Sequence names allowed to be inlined into SQL by this module.
///
/// `format!`-built SQL is unavoidable for sequence names (Postgres does not
/// accept them as bind parameters), so we gate it on an explicit allowlist
/// rather than trusting callers. Mirrors the `resolve_doc_column` pattern in
/// `employee_repo.rs`.
const ALLOWED_SEQUENCES: &[&str] = &["invoice_number_seq", "offer_number_seq"];

fn resolve_seq(seq: &str) -> Result<&'static str, ApiError> {
    ALLOWED_SEQUENCES
        .iter()
        .find(|s| **s == seq)
        .copied()
        .ok_or_else(|| ApiError::Internal(format!("unknown sequence: {seq}")))
}

/// Read the next value each sequence will hand out, without consuming it.
///
/// `last_value` is the most recently issued value; `is_called` is false only
/// for a freshly created sequence that has never had `nextval` called on it.
async fn next_for_seq(db: &PgPool, seq: &str) -> Result<i64, ApiError> {
    let seq = resolve_seq(seq)?;
    let (last_value, is_called): (i64, bool) =
        sqlx::query_as(&format!("SELECT last_value, is_called FROM {seq}"))
            .fetch_one(db)
            .await?;
    Ok(if is_called { last_value + 1 } else { last_value })
}

pub(crate) async fn get_next_numbers(db: &PgPool) -> Result<NextNumbers, ApiError> {
    Ok(NextNumbers {
        next_invoice_number: next_for_seq(db, "invoice_number_seq").await?,
        next_offer_number: next_for_seq(db, "offer_number_seq").await?,
    })
}

/// Set the next value a sequence will hand out. `setval(seq, n, false)` means
/// the following `nextval` returns exactly `n`. `seq` must be in
/// `ALLOWED_SEQUENCES` — `resolve_seq` rejects anything else.
async fn set_next_for_seq(db: &PgPool, seq: &str, n: i64) -> Result<(), ApiError> {
    let seq = resolve_seq(seq)?;
    sqlx::query(&format!("SELECT setval('{seq}', $1, false)"))
        .bind(n)
        .execute(db)
        .await?;
    Ok(())
}

pub(crate) async fn set_next_invoice(db: &PgPool, n: i64) -> Result<(), ApiError> {
    set_next_for_seq(db, "invoice_number_seq", n).await
}

pub(crate) async fn set_next_offer(db: &PgPool, n: i64) -> Result<(), ApiError> {
    set_next_for_seq(db, "offer_number_seq", n).await
}

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------

/// Return the value of the `agent_owns_approval` feature flag.
///
/// Defaults to `false` if the settings row is absent or cannot be parsed.
/// When `true`, the Telegram approval-post code path should be skipped and
/// the agent's event consumer handles approval routing instead.
#[allow(dead_code)]
pub(crate) async fn agent_owns_approval(db: &PgPool) -> bool {
    let result: Result<Option<(serde_json::Value,)>, _> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'agent_owns_approval'")
            .fetch_optional(db)
            .await;
    match result {
        Ok(Some((v,))) => v.as_bool().unwrap_or(false),
        _ => false,
    }
}
