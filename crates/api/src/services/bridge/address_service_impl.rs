//! Bridge impl for `AddressService`.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{AddressPatch, AddressService, DistanceResult, ServiceError};

pub struct AddressServiceImpl {
    pool: PgPool,
}

impl AddressServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AddressService for AddressServiceImpl {
    async fn get_distance(
        &self,
        from_address_id: Uuid,
        to_address_id: Uuid,
    ) -> Result<Option<DistanceResult>, ServiceError> {
        // Look up the stored distance from any inquiry that has these two addresses.
        let row: Option<(f64,)> = sqlx::query_as(
            r#"
            SELECT distance_km
            FROM inquiries
            WHERE origin_address_id = $1 AND destination_address_id = $2
              AND distance_km IS NOT NULL
            ORDER BY updated_at DESC
            LIMIT 1
            "#,
        )
        .bind(from_address_id)
        .bind(to_address_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(row.map(|(d,)| DistanceResult {
            from_address_id,
            to_address_id,
            distance_km: d,
            duration_minutes: None,
        }))
    }

    async fn update_inquiry_addresses(
        &self,
        inquiry_id: Uuid,
        from: Option<AddressPatch>,
        to: Option<AddressPatch>,
    ) -> Result<(), ServiceError> {
        // Fetch current address IDs.
        let row: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
            "SELECT origin_address_id, destination_address_id FROM inquiries WHERE id = $1",
        )
        .bind(inquiry_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (origin_id, dest_id) =
            row.ok_or_else(|| ServiceError::NotFound(format!("Anfrage {inquiry_id}")))?;

        if let (Some(patch), Some(addr_id)) = (from, origin_id) {
            apply_address_patch(&self.pool, addr_id, patch).await?;
        }
        if let (Some(patch), Some(addr_id)) = (to, dest_id) {
            apply_address_patch(&self.pool, addr_id, patch).await?;
        }

        // Reset distance — caller can re-trigger ORS.
        sqlx::query(
            "UPDATE inquiries SET distance_km = NULL, updated_at = NOW() WHERE id = $1",
        )
        .bind(inquiry_id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(())
    }
}

async fn apply_address_patch(
    pool: &PgPool,
    addr_id: Uuid,
    patch: AddressPatch,
) -> Result<(), ServiceError> {
    sqlx::query(
        r#"
        UPDATE addresses SET
            street       = COALESCE($2, street),
            city         = COALESCE($3, city),
            postal_code  = COALESCE($4, postal_code),
            country      = COALESCE($5, country),
            floor        = COALESCE($6, floor),
            elevator     = COALESCE($7, elevator)
        WHERE id = $1
        "#,
    )
    .bind(addr_id)
    .bind(patch.street.as_deref())
    .bind(patch.city.as_deref())
    .bind(patch.postal_code.as_deref())
    .bind(patch.country.as_deref())
    .bind(patch.floor.as_deref())
    .bind(patch.elevator)
    .execute(pool)
    .await
    .map_err(super::map_sqlx)?;
    // `house_number` is stored as part of `street` in this schema — ignored.
    Ok(())
}
