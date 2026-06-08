//! Bridge impl for `CustomerService`.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::models::{CustomerSnapshot, InquiryListItem};
use aust_core::services::{CustomerPatch, CustomerService, NewCustomer, ServiceError};

use crate::services::inquiry_builder;

pub struct CustomerServiceImpl {
    pool: PgPool,
}

impl CustomerServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[allow(clippy::too_many_arguments)]
fn row_to_snapshot(
    id: Uuid,
    salutation: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    customer_type: Option<String>,
    company_name: Option<String>,
) -> CustomerSnapshot {
    let name = match (first_name.as_deref(), last_name.as_deref()) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f.to_string()),
        (None, Some(l)) => Some(l.to_string()),
        (None, None) => None,
    };
    CustomerSnapshot { id, name, salutation, first_name, last_name, email, phone, customer_type, company_name }
}

#[async_trait]
impl CustomerService for CustomerServiceImpl {
    async fn get(&self, id: Uuid) -> Result<CustomerSnapshot, ServiceError> {
        let row: Option<(
            Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT id, salutation, first_name, last_name, email, phone,
                   customer_type, company_name
            FROM customers
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, salutation, first_name, last_name, email, phone, customer_type, company_name) =
            row.ok_or_else(|| ServiceError::NotFound(format!("Kunde {id}")))?;

        Ok(row_to_snapshot(id, salutation, first_name, last_name, email, phone, customer_type, company_name))
    }

    async fn create(&self, new: NewCustomer) -> Result<CustomerSnapshot, ServiceError> {
        // Validate customer_type against the DB CHECK (private|business) up front so
        // a bad value returns a clean German message instead of an opaque 500.
        let customer_type = match new.customer_type.as_deref().map(str::trim) {
            None | Some("") => "private",
            Some(t @ ("private" | "business")) => t,
            Some(other) => {
                return Err(ServiceError::Validation(format!(
                    "Ungültiger Kundentyp '{other}' (erlaubt: private, business)."
                )))
            }
        };

        let salutation = match new.salutation.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(s @ ("Herr" | "Frau" | "Divers")) => Some(s.to_string()),
            Some(other) => {
                return Err(ServiceError::Validation(format!(
                    "Ungültige Anrede '{other}' (erlaubt: Herr, Frau, Divers)."
                )))
            }
        };

        let first = new.first_name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let last = new.last_name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let company = new.company_name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let email = new.email.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let phone = new.phone.as_deref().map(str::trim).filter(|s| !s.is_empty());

        if first.is_none() && last.is_none() && company.is_none() {
            return Err(ServiceError::Validation(
                "Mindestens Vor-/Nachname oder Firmenname erforderlich.".to_string(),
            ));
        }

        let name = match (first, last) {
            (Some(f), Some(l)) => Some(format!("{f} {l}")),
            (Some(f), None) => Some(f.to_string()),
            (None, Some(l)) => Some(l.to_string()),
            (None, None) => company.map(str::to_string),
        };

        let id = Uuid::now_v7();
        let now = chrono::Utc::now();

        let row: (
            Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            r#"
            INSERT INTO customers
                (id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
            RETURNING id, salutation, first_name, last_name, email, phone, customer_type, company_name
            "#,
        )
        .bind(id)
        .bind(email)
        .bind(name.as_deref())
        .bind(salutation.as_deref())
        .bind(first)
        .bind(last)
        .bind(phone)
        .bind(customer_type)
        .bind(company)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, salutation, first_name, last_name, email, phone, customer_type, company_name) = row;
        Ok(row_to_snapshot(id, salutation, first_name, last_name, email, phone, customer_type, company_name))
    }

    async fn search(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<CustomerSnapshot>, ServiceError> {
        let pattern = format!("%{query}%");
        let limit_i = limit.min(50) as i64;

        let rows: Vec<(
            Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT id, salutation, first_name, last_name, email, phone,
                   customer_type, company_name
            FROM customers
            WHERE name ILIKE $1 OR email ILIKE $1 OR first_name ILIKE $1 OR last_name ILIKE $1
            ORDER BY last_name, first_name
            LIMIT $2
            "#,
        )
        .bind(&pattern)
        .bind(limit_i)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, salutation, first_name, last_name, email, phone, customer_type, company_name)| {
                row_to_snapshot(id, salutation, first_name, last_name, email, phone, customer_type, company_name)
            })
            .collect())
    }

    async fn list_inquiries_for(
        &self,
        customer_id: Uuid,
    ) -> Result<Vec<InquiryListItem>, ServiceError> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM inquiries WHERE customer_id = $1 ORDER BY created_at DESC LIMIT 50",
        )
        .bind(customer_id)
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

    async fn update(
        &self,
        id: Uuid,
        patch: CustomerPatch,
    ) -> Result<CustomerSnapshot, ServiceError> {
        sqlx::query(
            r#"
            UPDATE customers SET
                phone = COALESCE($2, phone),
                email = COALESCE($3, email),
                first_name = COALESCE($4, first_name),
                last_name = COALESCE($5, last_name),
                name = CASE
                    WHEN $4 IS NOT NULL OR $5 IS NOT NULL
                    THEN COALESCE($4, first_name) || ' ' || COALESCE($5, last_name)
                    ELSE name
                END
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(patch.phone.as_deref())
        .bind(patch.email.as_deref())
        .bind(patch.first_name.as_deref())
        .bind(patch.last_name.as_deref())
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        self.get(id).await
    }

    async fn add_note(&self, id: Uuid, text: &str) -> Result<(), ServiceError> {
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string();
        let note_line = format!("[{timestamp}] {text}");

        sqlx::query(
            r#"
            UPDATE customers SET
                notes = CASE WHEN notes IS NULL OR notes = '' THEN $1
                             ELSE notes || E'\n' || $1 END
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

    async fn merge(
        &self,
        keep_id: Uuid,
        merge_id: Uuid,
    ) -> Result<CustomerSnapshot, ServiceError> {
        // Run all reassignments and the sentinel update in a single transaction
        // so a partial failure leaves no orphaned rows.
        let mut tx = self.pool.begin().await.map_err(super::map_sqlx)?;

        // Reassign all direct customer-scoped child rows.
        sqlx::query("UPDATE inquiries SET customer_id = $1 WHERE customer_id = $2")
            .bind(keep_id).bind(merge_id)
            .execute(&mut *tx).await.map_err(super::map_sqlx)?;

        // customer_otps are email-scoped (not customer_id FK); they cannot be reassigned.
        // customer_sessions are customer_id FK; reassign.
        sqlx::query("UPDATE customer_sessions SET customer_id = $1 WHERE customer_id = $2")
            .bind(keep_id).bind(merge_id)
            .execute(&mut *tx).await.map_err(super::map_sqlx)?;

        sqlx::query("UPDATE email_threads SET customer_id = $1 WHERE customer_id = $2")
            .bind(keep_id).bind(merge_id)
            .execute(&mut *tx).await.map_err(super::map_sqlx)?;

        // Rewrite agent_memory scope references: 'customer:<merge_id>' → 'customer:<keep_id>'.
        let merge_scope = format!("customer:{merge_id}");
        let keep_scope  = format!("customer:{keep_id}");
        sqlx::query("UPDATE agent_memory SET scope = $1 WHERE scope = $2")
            .bind(&keep_scope).bind(&merge_scope)
            .execute(&mut *tx).await.map_err(super::map_sqlx)?;

        // Rewrite agent_episodes refs jsonb if it references the merged customer_id.
        // Replace {"customer_id": "<merge_id>"} → {"customer_id": "<keep_id>"} within refs.
        let merge_id_str = merge_id.to_string();
        let keep_id_str  = keep_id.to_string();
        sqlx::query(
            r#"
            UPDATE agent_episodes
               SET refs = refs - 'customer_id' || jsonb_build_object('customer_id', $1::text)
             WHERE refs->>'customer_id' = $2
            "#,
        )
        .bind(&keep_id_str).bind(&merge_id_str)
        .execute(&mut *tx).await.map_err(super::map_sqlx)?;

        // Mark the merged customer with the sentinel FK + a UI note.
        sqlx::query(
            r#"
            UPDATE customers
               SET merged_into = $2,
                   notes = COALESCE(notes, '') || ' [MERGED INTO ' || $2::text || ']'
             WHERE id = $1
            "#,
        )
        .bind(merge_id).bind(keep_id)
        .execute(&mut *tx).await.map_err(super::map_sqlx)?;

        tx.commit().await.map_err(super::map_sqlx)?;

        self.get(keep_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn try_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        sqlx::PgPool::connect(&url).await.ok()
    }

    /// Merge reassigns all child tables: customer_otps, customer_sessions, email_threads,
    /// inquiries, agent_memory scope, agent_episodes refs, and sets merged_into.
    #[tokio::test]
    async fn merge_reassigns_all_child_rows() {
        let Some(pool) = try_pool().await else { return };

        // Insert two customers.
        let keep_id = Uuid::now_v7();
        let merge_id = Uuid::now_v7();

        for (id, name) in [(keep_id, "Keep Kunde"), (merge_id, "Merge Kunde")] {
            sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, $2, $3)")
                .bind(id).bind(name).bind(format!("{id}@test.de"))
                .execute(&pool).await.expect("insert customer");
        }

        // Seed a customer_session for merge_id.
        // (customer_otps are email-scoped, not customer_id FK, so not reassignable.)
        let unique_token = format!("tok-{}", merge_id);
        sqlx::query("INSERT INTO customer_sessions (id, customer_id, token, expires_at) VALUES (gen_random_uuid(), $1, $2, NOW() + INTERVAL '1 day')")
            .bind(merge_id).bind(&unique_token).execute(&pool).await.expect("insert session");

        // Seed an email_thread for merge_id.
        sqlx::query("INSERT INTO email_threads (id, customer_id, subject) VALUES (gen_random_uuid(), $1, 'Test')")
            .bind(merge_id).execute(&pool).await.expect("insert thread");

        // Seed agent_memory scoped to merge_id.
        let merge_scope = format!("customer:{merge_id}");
        sqlx::query("INSERT INTO agent_memory (id, scope, kind, key, value, source, confidence) VALUES (gen_random_uuid(), $1, 'fact', 'testkey', '\"val\"'::jsonb, 'user_explicit', 1.0)")
            .bind(&merge_scope).execute(&pool).await.expect("insert memory");

        // Perform merge.
        let svc = CustomerServiceImpl::new(pool.clone());
        let snap = svc.merge(keep_id, merge_id).await.expect("merge");
        assert_eq!(snap.id, keep_id);

        // Verify customer_sessions reassigned.
        let (sess_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM customer_sessions WHERE customer_id = $1")
            .bind(keep_id).fetch_one(&pool).await.expect("sess count");
        assert!(sess_count >= 1, "sessions should be reassigned to keep_id");

        // Verify email_threads reassigned.
        let (thread_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM email_threads WHERE customer_id = $1")
            .bind(keep_id).fetch_one(&pool).await.expect("thread count");
        assert!(thread_count >= 1, "threads should be reassigned to keep_id");

        // Verify agent_memory scope rewritten.
        let keep_scope = format!("customer:{keep_id}");
        let (mem_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM agent_memory WHERE scope = $1")
            .bind(&keep_scope).fetch_one(&pool).await.expect("mem count");
        assert!(mem_count >= 1, "agent_memory scope should be rewritten to keep_id");

        // Verify merged customer has merged_into set.
        let (merged_into,): (Option<Uuid>,) = sqlx::query_as("SELECT merged_into FROM customers WHERE id = $1")
            .bind(merge_id).fetch_one(&pool).await.expect("merged_into");
        assert_eq!(merged_into, Some(keep_id), "merged customer must have merged_into = keep_id");

        // Cleanup.
        sqlx::query("DELETE FROM agent_memory WHERE scope = $1 OR scope = $2")
            .bind(&merge_scope).bind(&keep_scope)
            .execute(&pool).await.ok();
        sqlx::query("DELETE FROM customer_sessions WHERE customer_id = ANY($1)")
            .bind(&[keep_id, merge_id][..]).execute(&pool).await.ok();
        sqlx::query("DELETE FROM email_threads WHERE customer_id = ANY($1)")
            .bind(&[keep_id, merge_id][..]).execute(&pool).await.ok();
        sqlx::query("DELETE FROM customers WHERE id = ANY($1)")
            .bind(&[keep_id, merge_id][..]).execute(&pool).await.ok();
    }
}
