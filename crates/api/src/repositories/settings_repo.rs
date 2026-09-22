//! Repository for the `settings` table — runtime-editable configuration.
//!
//! Handles the standard pricing values (so they can be changed without a
//! redeploy) and the "next number" controls for the invoice/KVA sequences.

use std::collections::HashMap;

use chrono::Datelike;
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
const ALLOWED_SEQUENCES: &[&str] = &["offer_number_seq"];

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

/// The current calendar year — the book invoice numbers are drawn from right now.
///
/// Invoice numbers are per-year (see `invoice_number_counters`), so "the next invoice
/// number" is only meaningful for a given year, and the settings page always means the
/// one Alex is invoicing in today.
fn current_year() -> i32 {
    chrono::Utc::now().date_naive().year()
}

pub(crate) async fn get_next_numbers(db: &PgPool) -> Result<NextNumbers, ApiError> {
    Ok(NextNumbers {
        // Invoice numbers no longer come from a sequence: they restart at 1 each
        // January, so the counter is a per-year table rather than `invoice_number_seq`.
        next_invoice_number: crate::repositories::invoice_repo::peek_next_invoice_number(
            db,
            current_year(),
        )
        .await?,
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
    crate::repositories::invoice_repo::set_next_invoice_number(db, current_year(), n).await?;
    Ok(())
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

// ---------------------------------------------------------------------------
// KVA follow-up
// ---------------------------------------------------------------------------

/// Settings key holding the follow-up threshold in days.
const KEY_KVA_FOLLOWUP_DAYS: &str = "kva_followup_days";

/// Fallback when the settings row is missing. 6 days on Alex's request
/// (2026-08-28): waiting for the median decision time (21 days) meant chasing
/// customers who had already booked elsewhere.
pub(crate) const DEFAULT_KVA_FOLLOWUP_DAYS: i64 = 6;

/// How many days a KVA may stay undecided before it lands on the Nachfassliste.
///
/// **Caller**: `kva_followup_service`, the KVA-Buch route, the settings page.
/// **Why**: The threshold is deliberately a stored setting rather than a
/// live-recomputed median — a moving threshold would make the Telegram nag
/// non-deterministic and untestable. The computed median is shown alongside it
/// in the UI so Alex can tune the number himself.
pub(crate) async fn get_kva_followup_days(db: &PgPool) -> Result<i64, ApiError> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = $1")
            .bind(KEY_KVA_FOLLOWUP_DAYS)
            .fetch_optional(db)
            .await?;
    Ok(row
        .and_then(|(v,)| v.as_i64())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_KVA_FOLLOWUP_DAYS))
}

/// Persist the follow-up threshold. Values below 1 are rejected by the caller.
pub(crate) async fn set_kva_followup_days(db: &PgPool, days: i64) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at)
         VALUES ($1, $2, NOW())
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(KEY_KVA_FOLLOWUP_DAYS)
    .bind(serde_json::json!(days))
    .execute(db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Positionen catalogue
// ---------------------------------------------------------------------------

/// The scalar pricing setting a catalogue position fell back on before the
/// catalogue existed.
///
/// **Why**: `assembly_price`, `parking_ban_price`, `packing_price` and
/// `transporter_price` have been editable — and edited — for a long time. A
/// catalogue that ignored them would silently reset four live prices to their
/// code defaults the first time it was read.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LegacyPrice {
    Assembly,
    ParkingBan,
    Packing,
    Transporter,
}

impl LegacyPrice {
    fn euros(self, p: &PricingSettings) -> f64 {
        match self {
            LegacyPrice::Assembly => p.assembly_price,
            LegacyPrice::ParkingBan => p.parking_ban_price,
            LegacyPrice::Packing => p.packing_price,
            LegacyPrice::Transporter => p.transporter_price,
        }
    }
}

/// One fixed position Alex can put on a KVA.
pub(crate) struct PositionDef {
    /// Stable id — used in the `settings` key and in the API, never shown.
    pub key: &'static str,
    /// What the KVA prints. Also the label the Positionen dropdown offers.
    pub label: &'static str,
    /// Default remark, pre-filled when the position is picked.
    pub remark: &'static str,
    /// Price before anyone edited anything.
    pub default_cents: i64,
    /// Pre-catalogue settings key, consulted before `default_cents`.
    pub legacy: Option<LegacyPrice>,
}

/// Every fixed position, in the order the KVA lists them.
///
/// **Caller**: `get_positions`, and through it both the settings page and the
/// Positionen panel on an inquiry.
/// **Why**: the prices used to live in a hardcoded list in the frontend, which
/// is exactly what feedback report ce764f7b is about — Alex could not change
/// them anywhere. Labels and remarks stay in code: they have to match the KVA
/// template rows, and the report asked for prices.
pub(crate) const POSITION_CATALOG: &[PositionDef] = &[
    PositionDef { key: "demontage", label: "Demontage", remark: "", default_cents: 2500, legacy: Some(LegacyPrice::Assembly) },
    PositionDef { key: "montage", label: "Montage", remark: "", default_cents: 2500, legacy: Some(LegacyPrice::Assembly) },
    PositionDef { key: "einpackservice", label: "Einpackservice", remark: "je Karton (Glas, Porzellan)", default_cents: 0, legacy: None },
    PositionDef { key: "halteverbotszone", label: "Halteverbotszone", remark: "", default_cents: 10_000, legacy: Some(LegacyPrice::ParkingBan) },
    PositionDef { key: "umzugsmaterial", label: "Umzugsmaterial", remark: "Stretchfolie, Decken, Gurte", default_cents: 3_000, legacy: Some(LegacyPrice::Packing) },
    PositionDef { key: "verkauf_seidenpapier", label: "Verkauf Seidenpapier", remark: "500x750", default_cents: 500, legacy: None },
    PositionDef { key: "verkauf_u_karton", label: "Verkauf U-Karton", remark: "590x318x328", default_cents: 210, legacy: None },
    PositionDef { key: "verkauf_b_karton", label: "Verkauf B-Karton", remark: "400x318x328", default_cents: 220, legacy: None },
    PositionDef { key: "fernsehkarton", label: "Fernsehkarton", remark: "", default_cents: 0, legacy: None },
    PositionDef { key: "verleih_kleiderboxen", label: "Verleih Kleiderboxen", remark: "610x520x1370", default_cents: 1_000, legacy: None },
    PositionDef { key: "transporter_3_5t", label: "3,5t Transporter m. Koffer", remark: "", default_cents: 6_000, legacy: Some(LegacyPrice::Transporter) },
    PositionDef { key: "moebellift", label: "Möbellift", remark: "", default_cents: 0, legacy: None },
    PositionDef { key: "transferfahrzeug", label: "Transferfahrzeug", remark: "", default_cents: 0, legacy: None },
];

/// A catalogue position with its effective price.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PositionPrice {
    pub key: String,
    pub label: String,
    pub remark: String,
    pub unit_price_cents: i64,
}

/// `settings` key holding one position's price.
fn position_key(key: &str) -> String {
    format!("position_price.{key}")
}

/// Load the catalogue with effective prices: a `position_price.*` row wins,
/// then the legacy scalar setting, then the code default.
///
/// **Caller**: the admin settings route, and `offer_builder` when it generates
/// the service line items.
pub(crate) async fn get_positions(
    db: &PgPool,
    config: &Config,
) -> Result<Vec<PositionPrice>, ApiError> {
    let pricing = get_pricing(db, config).await?;
    let rows: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT key, value FROM settings WHERE key LIKE 'position_price.%'")
            .fetch_all(db)
            .await?;
    let map: HashMap<String, serde_json::Value> = rows.into_iter().collect();

    Ok(POSITION_CATALOG
        .iter()
        .map(|d| {
            let stored = map.get(&position_key(d.key)).and_then(|v| v.as_i64());
            let unit_price_cents = stored.unwrap_or_else(|| match d.legacy {
                Some(l) => (l.euros(&pricing) * 100.0).round() as i64,
                None => d.default_cents,
            });
            PositionPrice {
                key: d.key.to_string(),
                label: d.label.to_string(),
                remark: d.remark.to_string(),
                unit_price_cents,
            }
        })
        .collect())
}

/// Persist prices for the given catalogue keys. Unknown keys are rejected
/// rather than silently written, so a typo cannot create a dead settings row.
pub(crate) async fn upsert_positions(
    db: &PgPool,
    prices: &[(String, i64)],
) -> Result<(), ApiError> {
    for (key, cents) in prices {
        if !POSITION_CATALOG.iter().any(|d| d.key == key) {
            return Err(ApiError::Validation(format!("Unbekannte Position: {key}")));
        }
        if *cents < 0 {
            return Err(ApiError::Validation(
                "Preise duerfen nicht negativ sein".into(),
            ));
        }
    }
    for (key, cents) in prices {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at)
             VALUES ($1, $2, NOW())
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
        )
        .bind(position_key(key))
        .bind(serde_json::json!(cents))
        .execute(db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upsert one raw settings row, bypassing the typed helpers.
    async fn put(db: &PgPool, key: &str, value: serde_json::Value) {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, NOW())
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(key)
        .bind(value)
        .execute(db)
        .await
        .unwrap();
    }

    fn cents_of(positions: &[PositionPrice], key: &str) -> i64 {
        positions.iter().find(|p| p.key == key).unwrap().unit_price_cents
    }

    /// Nobody has touched the catalogue yet, so the four positions that had a
    /// scalar setting keep the price Alex configured there — a catalogue that
    /// ignored them would quietly reset live prices to the code defaults.
    #[sqlx::test(migrations = "../../migrations")]
    async fn an_untouched_position_keeps_its_legacy_price(pool: PgPool) {
        put(&pool, "assembly_price", serde_json::json!(33.0)).await;
        put(&pool, "parking_ban_price", serde_json::json!(120.0)).await;

        let config = crate::test_helpers::test_config();
        let positions = get_positions(&pool, &config).await.unwrap();

        assert_eq!(cents_of(&positions, "demontage"), 3300);
        assert_eq!(cents_of(&positions, "montage"), 3300);
        assert_eq!(cents_of(&positions, "halteverbotszone"), 12_000);
        // No legacy key and no stored row: the code default stands.
        assert_eq!(cents_of(&positions, "verkauf_u_karton"), 210);
    }

    /// A saved price wins over the legacy setting, and Demontage/Montage are
    /// independent even though they shared one `assembly_price` before.
    #[sqlx::test(migrations = "../../migrations")]
    async fn a_saved_price_wins_and_de_montage_split_apart(pool: PgPool) {
        put(&pool, "assembly_price", serde_json::json!(33.0)).await;
        upsert_positions(&pool, &[("demontage".into(), 4000)])
            .await
            .unwrap();

        let config = crate::test_helpers::test_config();
        let positions = get_positions(&pool, &config).await.unwrap();
        assert_eq!(cents_of(&positions, "demontage"), 4000);
        assert_eq!(cents_of(&positions, "montage"), 3300);

        // Zero is a legitimate price (Möbellift is quoted on request).
        upsert_positions(&pool, &[("verleih_kleiderboxen".into(), 0)])
            .await
            .unwrap();
        let positions = get_positions(&pool, &config).await.unwrap();
        assert_eq!(cents_of(&positions, "verleih_kleiderboxen"), 0);
    }

    /// A typo must not create a settings row nobody ever reads again.
    #[sqlx::test(migrations = "../../migrations")]
    async fn an_unknown_key_or_a_negative_price_is_refused(pool: PgPool) {
        assert!(matches!(
            upsert_positions(&pool, &[("gibt_es_nicht".into(), 100)]).await,
            Err(ApiError::Validation(_))
        ));
        assert!(matches!(
            upsert_positions(&pool, &[("montage".into(), -1)]).await,
            Err(ApiError::Validation(_))
        ));

        // The valid entry of a rejected batch must not have been written either.
        assert!(matches!(
            upsert_positions(&pool, &[("montage".into(), 9900), ("gibt_es_nicht".into(), 1)]).await,
            Err(ApiError::Validation(_))
        ));
        let stored: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT value FROM settings WHERE key = 'position_price.montage'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(stored.is_none(), "a rejected batch must write nothing");
    }
}
