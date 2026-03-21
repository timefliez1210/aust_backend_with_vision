use chrono::{DateTime, Utc};
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
/// `clock_in` and `clock_out` must be ISO 8601 strings with timezone offset (e.g. "2026-03-15T07:00:00+01:00").
/// Derived `actual_hours` = (clock_out − clock_in) in hours, computed by the API.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateAssignment {
    pub planned_hours: Option<f64>,
    pub clock_in: Option<DateTime<Utc>>,
    pub clock_out: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}
