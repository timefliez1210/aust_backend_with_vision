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
    #[serde(default, deserialize_with = "deserialize_lenient_time")]
    pub clock_in: Option<NaiveTime>,
    #[serde(default, deserialize_with = "deserialize_lenient_time")]
    pub clock_out: Option<NaiveTime>,
    #[serde(default, deserialize_with = "deserialize_lenient_time")]
    pub start_time: Option<NaiveTime>,
    #[serde(default, deserialize_with = "deserialize_lenient_time")]
    pub end_time: Option<NaiveTime>,
    pub break_minutes: Option<i32>,
    pub actual_hours: Option<f64>,
    pub notes: Option<String>,
    /// When set, scopes the update to the single day at this date (multi-day inquiries).
    /// When omitted, updates day_number = 1 and the flat table (legacy single-day path).
    pub day_date: Option<chrono::NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub travel_costs_cents: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accommodation_cents: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meal_deduction: Option<String>,
}

/// Lenient time deserialization for assignment time fields.
///
/// Accepts `"HH:MM:SS"`, `"HH:MM"`, `"H:MM"`, and German decimal style
/// `"7.30"` / `"7,30"` (mobile `inputmode="decimal"` keyboards have no colon
/// key). Strict `Option<NaiveTime>` parsing previously turned such input into
/// a bare 422 before the handler ran — saves silently failed in the admin UI
/// (2026-06-10 incident: hours on Lisa Lullies' job wouldn't stick).
pub fn deserialize_lenient_time<'de, D>(de: D) -> Result<Option<NaiveTime>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::deserialize(de)?;
    let Some(s) = raw else { return Ok(None) };
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    parse_lenient_time(s).map(Some).ok_or_else(|| {
        serde::de::Error::custom(format!("ungültige Uhrzeit {s:?} (erwartet z. B. 07:30)"))
    })
}

fn parse_lenient_time(s: &str) -> Option<NaiveTime> {
    for fmt in ["%H:%M:%S", "%H:%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, fmt) {
            return Some(t);
        }
    }
    // German decimal separators: "7.30" / "7,30" → "7:30"
    let normalized = s.replace([',', '.'], ":");
    if normalized != s {
        return NaiveTime::parse_from_str(&normalized, "%H:%M").ok();
    }
    None
}

#[cfg(test)]
mod lenient_time_tests {
    use super::*;

    #[test]
    fn parses_all_admin_input_styles() {
        let expected = NaiveTime::from_hms_opt(7, 30, 0).unwrap();
        for input in ["07:30:00", "07:30", "7:30", "7.30", "7,30", "07.30"] {
            assert_eq!(parse_lenient_time(input), Some(expected), "input {input:?}");
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_lenient_time("morgen"), None);
        assert_eq!(parse_lenient_time("7.99"), None);
        assert_eq!(parse_lenient_time("25:00"), None);
    }

    #[test]
    fn update_assignment_deserializes_german_decimal_times() {
        let body: UpdateAssignment =
            serde_json::from_str(r#"{"clock_in":"7.30","clock_out":"12.30"}"#).unwrap();
        assert_eq!(body.clock_in, NaiveTime::from_hms_opt(7, 30, 0));
        assert_eq!(body.clock_out, NaiveTime::from_hms_opt(12, 30, 0));
        assert!(body.start_time.is_none());
    }

    #[test]
    fn update_assignment_rejects_invalid_time_with_clear_error() {
        let err = serde_json::from_str::<UpdateAssignment>(r#"{"clock_in":"7.99"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("ungültige Uhrzeit"), "got: {err}");
    }
}
