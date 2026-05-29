//! Bridge impl for `InquiryService`.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::models::{InquiryListItem, InquiryResponse, InquiryStatus, Services};
use aust_core::services::{InquiryService, ServiceError};

use crate::services::inquiry_builder;
use crate::ApiError;

pub struct InquiryServiceImpl {
    pool: PgPool,
}

impl InquiryServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl InquiryService for InquiryServiceImpl {
    async fn get_inquiry(&self, id: Uuid) -> Result<InquiryResponse, ServiceError> {
        match inquiry_builder::build_inquiry_response(&self.pool, id).await {
            Ok(r) => Ok(r),
            Err(ApiError::NotFound(msg)) => Err(ServiceError::NotFound(msg)),
            Err(ApiError::BadRequest(msg)) => Err(ServiceError::Validation(msg)),
            Err(other) => Err(ServiceError::Db(anyhow::anyhow!(other.to_string()))),
        }
    }

    async fn list_inquiries(
        &self,
        status_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<InquiryListItem>, ServiceError> {
        // Use a lightweight direct query — list_inquiries builder requires more args.
        let limit_i = limit.min(200) as i64;
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"
            SELECT id
            FROM inquiries
            WHERE ($1::TEXT IS NULL OR status::text = $1)
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(status_filter)
        .bind(limit_i)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let mut out = Vec::with_capacity(rows.len());
        for (id,) in rows {
            // Reuse the canonical builder for each id — small N (<= 200) keeps this cheap.
            if let Ok(resp) = inquiry_builder::build_inquiry_response(&self.pool, id).await {
                out.push(InquiryListItem {
                    id: resp.id,
                    customer_name: resp.customer.as_ref().and_then(|c| c.name.clone()),
                    customer_email: resp.customer.as_ref().and_then(|c| c.email.clone()),
                    salutation: resp.customer.as_ref().and_then(|c| c.salutation.clone()),
                    origin_city: resp.origin_address.as_ref().map(|a| a.city.clone()),
                    destination_city: resp.destination_address.as_ref().map(|a| a.city.clone()),
                    volume_m3: resp.volume_m3,
                    distance_km: resp.distance_km,
                    status: resp.status,
                    has_offer: resp.offer.is_some(),
                    offer_status: resp.offer.as_ref().map(|o| o.status.clone()),
                    service_type: resp.service_type.clone(),
                    customer_type: resp.customer.as_ref().and_then(|c| c.customer_type.clone()),
                    created_at: resp.created_at,
                });
            }
        }
        Ok(out)
    }

    async fn search_inquiries(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<InquiryListItem>, ServiceError> {
        let pattern = format!("%{query}%");
        let limit_i = limit.min(50) as i64;

        // Search across customer name, email, origin/destination city and notes.
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            r#"
            SELECT DISTINCT i.id
            FROM inquiries i
            LEFT JOIN customers c ON i.customer_id = c.id
            LEFT JOIN addresses oa ON i.origin_address_id = oa.id
            LEFT JOIN addresses da ON i.destination_address_id = da.id
            WHERE c.name ILIKE $1
               OR c.email ILIKE $1
               OR oa.city ILIKE $1
               OR da.city ILIKE $1
               OR i.notes ILIKE $1
            ORDER BY i.id DESC
            LIMIT $2
            "#,
        )
        .bind(&pattern)
        .bind(limit_i)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let mut out = Vec::with_capacity(rows.len());
        for (id,) in rows {
            if let Ok(resp) = inquiry_builder::build_inquiry_response(&self.pool, id).await {
                out.push(InquiryListItem {
                    id: resp.id,
                    customer_name: resp.customer.as_ref().and_then(|c| c.name.clone()),
                    customer_email: resp.customer.as_ref().and_then(|c| c.email.clone()),
                    salutation: resp.customer.as_ref().and_then(|c| c.salutation.clone()),
                    origin_city: resp.origin_address.as_ref().map(|a| a.city.clone()),
                    destination_city: resp.destination_address.as_ref().map(|a| a.city.clone()),
                    volume_m3: resp.volume_m3,
                    distance_km: resp.distance_km,
                    status: resp.status,
                    has_offer: resp.offer.is_some(),
                    offer_status: resp.offer.as_ref().map(|o| o.status.clone()),
                    service_type: resp.service_type.clone(),
                    customer_type: resp.customer.as_ref().and_then(|c| c.customer_type.clone()),
                    created_at: resp.created_at,
                });
            }
        }
        Ok(out)
    }

    async fn add_note(
        &self,
        id: Uuid,
        text: &str,
        author_role: &str,
    ) -> Result<(), ServiceError> {
        // Append to the notes field with a timestamp prefix.
        let timestamp = Utc::now().format("%Y-%m-%d %H:%M").to_string();
        let note_line = format!("[{timestamp}] [{author_role}] {text}");

        sqlx::query(
            r#"
            UPDATE inquiries
            SET notes = CASE
                WHEN notes IS NULL OR notes = '' THEN $1
                ELSE notes || E'\n' || $1
            END,
            updated_at = NOW()
            WHERE id = $2
            "#,
        )
        .bind(&note_line)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(())
    }

    async fn update_status(
        &self,
        id: Uuid,
        new_status: &str,
        _reason: Option<&str>,
    ) -> Result<InquiryResponse, ServiceError> {
        // Fetch current status.
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status::text FROM inquiries WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(super::map_sqlx)?;

        let current_str = row
            .ok_or_else(|| ServiceError::NotFound(format!("Anfrage {id}")))?
            .0;

        let current: InquiryStatus = current_str
            .parse()
            .map_err(|_| ServiceError::Validation(format!("Ungültiger Status: {current_str}")))?;

        let target: InquiryStatus = new_status
            .parse()
            .map_err(|_| ServiceError::Validation(format!("Ungültiger Zielstatus: {new_status}")))?;

        if !current.can_transition_to(&target) {
            return Err(ServiceError::Validation(format!(
                "Statusübergang von '{current}' nach '{target}' nicht erlaubt"
            )));
        }

        sqlx::query("UPDATE inquiries SET status = $1, updated_at = NOW() WHERE id = $2")
            .bind(new_status)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;

        match inquiry_builder::build_inquiry_response(&self.pool, id).await {
            Ok(r) => Ok(r),
            Err(ApiError::NotFound(msg)) => Err(ServiceError::NotFound(msg)),
            Err(other) => Err(ServiceError::Db(anyhow::anyhow!(other.to_string()))),
        }
    }

    async fn set_services(
        &self,
        id: Uuid,
        services: Services,
    ) -> Result<(), ServiceError> {
        let services_json = serde_json::to_value(&services)
            .map_err(|e| ServiceError::Validation(format!("Serialisierungsfehler: {e}")))?;

        sqlx::query(
            "UPDATE inquiries SET services = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(&services_json)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(())
    }

    async fn cancel_inquiry(&self, id: Uuid, reason: &str) -> Result<(), ServiceError> {
        self.update_status(id, "cancelled", Some(reason)).await?;
        Ok(())
    }
}
