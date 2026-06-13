//! Bridge impl for `EmployeeService`.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::PgPool;
use uuid::Uuid;

use aust_core::services::{
    EmployeePatch, EmployeeRecord, EmployeeService, EmployeeWorkloadEntry, ServiceError,
};

pub struct EmployeeServiceImpl {
    pool: PgPool,
}

impl EmployeeServiceImpl {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EmployeeService for EmployeeServiceImpl {
    async fn list(&self, active_only: bool) -> Result<Vec<EmployeeRecord>, ServiceError> {
        let active_filter: Option<bool> = if active_only { Some(true) } else { None };
        let rows: Vec<(Uuid, String, String, String, Option<String>, String, bool)> = sqlx::query_as(
            r#"
            SELECT id, first_name, last_name, email, phone, role, active
            FROM employees
            WHERE ($1::bool IS NULL OR active = $1)
            ORDER BY last_name, first_name
            LIMIT 200
            "#,
        )
        .bind(active_filter)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(id, first_name, last_name, email, phone, role, active)| EmployeeRecord {
                id,
                first_name,
                last_name,
                email: Some(email),
                phone,
                role: Some(role),
                active,
            })
            .collect())
    }

    async fn get(&self, id: Uuid) -> Result<EmployeeRecord, ServiceError> {
        let row: Option<(Uuid, String, String, String, Option<String>, String, bool)> = sqlx::query_as(
            r#"
            SELECT id, first_name, last_name, email, phone, role, active
            FROM employees
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        let (id, first_name, last_name, email, phone, role, active) =
            row.ok_or_else(|| ServiceError::NotFound(format!("Mitarbeiter {id}")))?;

        Ok(EmployeeRecord {
            id,
            first_name,
            last_name,
            email: Some(email),
            phone,
            role: Some(role),
            active,
        })
    }

    async fn get_workload(
        &self,
        id: Uuid,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EmployeeWorkloadEntry>, ServiceError> {
        type WorkloadRow = (
            NaiveDate,
            Option<Uuid>,
            Option<Uuid>,
            String,
            String,
            Option<chrono::NaiveTime>,
            Option<chrono::NaiveTime>,
            Option<f64>,
        );
        let rows: Vec<WorkloadRow> = sqlx::query_as(
            r#"
            SELECT ie.job_date, ie.inquiry_id, NULL::uuid AS calendar_item_id,
                   COALESCE(c.name, 'Umzug') AS title, 'moving'::text AS category,
                   ie.clock_in, ie.clock_out, ie.actual_hours::float8 AS actual_hours
            FROM inquiry_employees ie
            JOIN inquiries i ON ie.inquiry_id = i.id
            LEFT JOIN customers c ON i.customer_id = c.id
            WHERE ie.employee_id = $1 AND ie.job_date BETWEEN $2 AND $3
            UNION ALL
            SELECT cie.job_date, NULL::uuid, cie.calendar_item_id,
                   ci.title, ci.category,
                   cie.clock_in, cie.clock_out, cie.actual_hours::float8
            FROM calendar_item_employees cie
            JOIN calendar_items ci ON cie.calendar_item_id = ci.id
            WHERE cie.employee_id = $1 AND cie.job_date BETWEEN $2 AND $3
            ORDER BY job_date
            "#,
        )
        .bind(id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(super::map_sqlx)?;

        Ok(rows
            .into_iter()
            .map(|(date, inquiry_id, calendar_item_id, title, category, clock_in, clock_out, actual_hours)| {
                EmployeeWorkloadEntry {
                    date,
                    inquiry_id,
                    calendar_item_id,
                    title,
                    category,
                    clock_in,
                    clock_out,
                    actual_hours,
                }
            })
            .collect())
    }

    async fn update(&self, id: Uuid, patch: EmployeePatch) -> Result<EmployeeRecord, ServiceError> {
        sqlx::query(
            r#"
            UPDATE employees SET
                phone = COALESCE($2, phone),
                role  = COALESCE($3, role)
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(patch.phone.as_deref())
        .bind(patch.role.as_deref())
        .execute(&self.pool)
        .await
        .map_err(super::map_sqlx)?;
        self.get(id).await
    }

    async fn set_active(&self, id: Uuid, active: bool) -> Result<(), ServiceError> {
        sqlx::query("UPDATE employees SET active = $1 WHERE id = $2")
            .bind(active)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(super::map_sqlx)?;
        Ok(())
    }
}
