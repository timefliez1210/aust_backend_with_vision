use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Full employee record.
///
/// **Caller**: Admin employee endpoints, inquiry assignment queries
/// **Why**: Core domain model for employee management and hours tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Employee {
    pub id: Uuid,
    pub salutation: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub monthly_hours_target: f64,
    pub active: bool,
    /// S3 key for the uploaded Arbeitsvertrag PDF. Null if not yet uploaded.
    pub arbeitsvertrag_key: Option<String>,
    /// S3 key for the uploaded Mitarbeiterfragebogen. Null if not yet uploaded.
    pub mitarbeiterfragebogen_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new employee.
///
/// **Caller**: `POST /api/v1/admin/employees`
/// **Why**: Validated input struct for employee creation
#[derive(Debug, Deserialize)]
pub struct CreateEmployee {
    pub salutation: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub monthly_hours_target: Option<f64>,
}

/// Input for updating an existing employee.
///
/// **Caller**: `PATCH /api/v1/admin/employees/{id}`
/// **Why**: All fields optional for partial update
#[derive(Debug, Default, Deserialize)]
pub struct UpdateEmployee {
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub monthly_hours_target: Option<f64>,
    pub active: Option<bool>,
}

/// Input for assigning an employee to an inquiry.
///
/// **Caller**: `POST /api/v1/inquiries/{id}/employees`
/// **Why**: Associates an employee with a moving job
#[derive(Debug, Deserialize)]
pub struct AssignEmployee {
    pub employee_id: Uuid,
    pub planned_hours: f64,
    pub notes: Option<String>,
}

/// Input for updating an assignment.
///
/// **Caller**: `PATCH /api/v1/inquiries/{id}/employees/{emp_id}`
/// **Why**: Allows updating planned hours and clock-in/clock-out times for actual hours tracking.
/// `clock_in` and `clock_out` are TIME values (HH:MM:SS format, e.g. "07:00:00").
/// `actual_hours` can be provided as a manual override; if null and both clock times are set,
/// it is derived as (clock_out − clock_in) in hours minus break_minutes/60.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateAssignment {
    pub planned_hours: Option<f64>,
    pub clock_in: Option<NaiveTime>,
    pub clock_out: Option<NaiveTime>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub break_minutes: Option<i32>,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
    /// When set, scopes the update to the single day at this date (multi-day inquiries).
    /// When omitted, updates day_number = 1 and the flat table (legacy single-day path).
    pub day_date: Option<chrono::NaiveDate>,
}
