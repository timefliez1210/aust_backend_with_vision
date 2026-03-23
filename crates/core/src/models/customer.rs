use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

/// Detect the appropriate address-block salutation and formal greeting line from a
/// legacy customer name string that may include an explicit "Herr"/"Frau" prefix.
///
/// **Caller**: `resolve_greeting` (fallback branch) and any caller that only has a
/// plain name string and needs both the salutation token and the greeting line.
/// **Why**: Pre-structured-name customers were stored with a single `name` field that
/// sometimes includes the title prefix (e.g. `"Herr Müller"`). This heuristic
/// recovers the salutation from that prefix, then falls back to a lookup table of
/// common German/Austrian female first names.
///
/// # Parameters
/// - `name` — raw customer name string (may include "Herr"/"Frau" prefix, may have
///   leading/trailing whitespace)
///
/// # Returns
/// `(salutation_token, greeting_line)` — e.g.
/// `("Herrn", "Sehr geehrter Herr Müller,")` or
/// `("", "Sehr geehrte Damen und Herren,")` for a single-word name.
pub fn detect_salutation_from_name(name: &str) -> (String, String) {
    let name_trimmed = name.trim();
    if name_trimmed.starts_with("Frau ") {
        let after = name_trimmed.strip_prefix("Frau ").unwrap().trim();
        return (
            "Frau".to_string(),
            format!("Sehr geehrte Frau {after},"),
        );
    }
    if name_trimmed.starts_with("Herr ") {
        let after = name_trimmed.strip_prefix("Herr ").unwrap().trim();
        return (
            "Herrn".to_string(),
            format!("Sehr geehrter Herr {after},"),
        );
    }

    let first_name = name_trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    let last_name = name_trimmed
        .split_whitespace()
        .last()
        .unwrap_or(name_trimmed);

    // Common German/Austrian female first names.
    const FEMALE_NAMES: &[&str] = &[
        "anna", "andrea", "angelika", "anita", "barbara", "birgit", "brigitte",
        "carina", "carmen", "caroline", "charlotte", "christa", "christina", "claudia",
        "daniela", "diana", "doris", "elisabeth", "elena", "elke", "emma", "erika",
        "eva", "franziska", "gabriele", "gabi", "gertrud", "gisela", "hannah",
        "heidi", "helga", "ines", "ingrid", "irene", "jana", "jessica", "johanna",
        "julia", "karin", "katharina", "katrin", "kristina", "laura", "lena", "lisa",
        "luisa", "manuela", "maria", "marie", "marina", "marion", "marlene",
        "martina", "melanie", "michaela", "monika", "nadine", "natalie", "nicole",
        "nina", "olivia", "patricia", "petra", "renate", "rita", "rosa", "ruth",
        "sabine", "sandra", "sara", "sarah", "silvia", "simone", "sofia", "sophie",
        "stefanie", "stephanie", "susanne", "sylvia", "tanja", "teresa", "theresia",
        "ursula", "ute", "valentina", "vanessa", "vera", "verena", "veronika",
    ];

    let is_female = FEMALE_NAMES.contains(&first_name.as_str());

    if name_trimmed.contains(' ') {
        if is_female {
            (
                "Frau".to_string(),
                format!("Sehr geehrte Frau {last_name},"),
            )
        } else {
            (
                "Herrn".to_string(),
                format!("Sehr geehrter Herr {last_name},"),
            )
        }
    } else {
        (
            String::new(),
            "Sehr geehrte Damen und Herren,".to_string(),
        )
    }
}

/// Build the formal greeting line from structured customer fields.
///
/// **Caller**: `Customer::formal_greeting`, `CustomerRow::formal_greeting` in
/// `crates/api/src/routes/offers.rs`, and any code that builds an email or PDF
/// greeting from stored customer data.
/// **Why**: Single source of truth so that the `Customer` domain model and the
/// `CustomerRow` SQL projection always produce the same greeting string.
///
/// Priority:
/// 1. Explicit `salutation` + `last_name` → deterministic template
/// 2. `salutation` only (no last name) → generic fallback
/// 3. `name` string present → `detect_salutation_from_name` heuristic
/// 4. No usable data → `"Sehr geehrte Damen und Herren,"`
///
/// # Parameters
/// - `salutation` — stored salutation token: `"Herr"`, `"Frau"`, `"D"`, or `None`
/// - `first_name` — given name (currently unused but accepted for signature symmetry)
/// - `last_name` — family name; used in `"Sehr geehrter Herr {last_name},"`
/// - `name` — legacy full-name string; used only when structured fields are absent
///
/// # Returns
/// German formal greeting string ending with a comma,
/// e.g. `"Sehr geehrter Herr Müller,"`.
pub fn resolve_greeting(
    salutation: Option<&str>,
    _first_name: Option<&str>,
    last_name: Option<&str>,
    name: Option<&str>,
) -> String {
    match (salutation, last_name) {
        (Some("Herr"), Some(ln)) => format!("Sehr geehrter Herr {ln},"),
        (Some("Frau"), Some(ln)) => format!("Sehr geehrte Frau {ln},"),
        (Some("D"),    Some(ln)) => format!("Sehr geehrte Person {ln},"),
        _ => {
            // Fall back to name-string heuristic for legacy records.
            if let Some(n) = name {
                detect_salutation_from_name(n).1
            } else {
                "Sehr geehrte Damen und Herren,".to_string()
            }
        }
    }
}

/// Derive the address-block salutation token from structured customer fields.
///
/// **Caller**: `Customer::address_salutation`, `CustomerRow::address_salutation` in
/// `crates/api/src/routes/offers.rs`.
/// **Why**: The XLSX offer template writes a salutation token (e.g. "Herrn") into
/// cell A8. Centralising this logic avoids drift between the two customer types.
///
/// # Parameters
/// - `salutation` — stored salutation token: `"Herr"`, `"Frau"`, `"D"`, or `None`
/// - `name` — legacy full-name string; used only when `salutation` is `None`
///
/// # Returns
/// `"Herrn"`, `"Frau"`, `"Divers"`, or `""` (empty string when unknown).
pub fn resolve_address_salutation(salutation: Option<&str>, name: Option<&str>) -> String {
    match salutation {
        Some("Herr") => "Herrn".to_string(),
        Some("Frau") => "Frau".to_string(),
        Some("D")    => "Divers".to_string(),
        _ => {
            if let Some(n) = name {
                detect_salutation_from_name(n).0
            } else {
                String::new()
            }
        }
    }
}

/// A moving-company customer, identified uniquely by email address.
///
/// Customers are created (or upserted) by the orchestrator the first time a
/// moving inquiry arrives for a given email. All quotes and offers reference
/// a `Customer` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Customer {
    /// UUID v7 primary key (time-ordered for efficient B-tree indexing).
    pub id: Uuid,
    /// Unique email address; used as the primary business identifier.
    pub email: String,
    /// Full display name (Vorname + Nachname); kept for display and backwards compat.
    pub name: Option<String>,
    /// Explicit salutation chosen by the customer: "Herr", "Frau", or "D" (divers).
    /// When present, always used verbatim — never guessed from the name.
    pub salutation: Option<String>,
    /// Given name (Vorname).
    pub first_name: Option<String>,
    /// Family name (Nachname); used in formal greetings ("Sehr geehrter Herr Müller").
    pub last_name: Option<String>,
    /// Phone number; used for follow-up calls and offer delivery confirmations.
    pub phone: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Customer {
    /// Build the formal greeting line from stored fields, e.g.
    /// `"Sehr geehrter Herr Müller,"` or `"Sehr geehrte Frau Schmidt,"`.
    /// Delegates to `resolve_greeting`; falls back through the name-string heuristic
    /// and ultimately to `"Sehr geehrte Damen und Herren,"`.
    pub fn formal_greeting(&self) -> String {
        resolve_greeting(
            self.salutation.as_deref(),
            self.first_name.as_deref(),
            self.last_name.as_deref(),
            self.name.as_deref(),
        )
    }

    /// Address-block salutation for the XLSX offer (cell A8).
    /// Returns `"Herrn"`, `"Frau"`, `"Divers"`, or `""`.
    pub fn address_salutation(&self) -> String {
        resolve_address_salutation(self.salutation.as_deref(), self.name.as_deref())
    }
}

/// Input for creating a new customer record.
///
/// **Caller**: `orchestrator.rs` calls the customer repository with this struct
/// when processing a new `MovingInquiry`.
/// **Why**: Separating creation input from the full `Customer` model keeps
/// validation logic close to the input boundary and prevents callers from
/// accidentally supplying server-generated fields like `id` or timestamps.
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateCustomer {
    /// Must be a syntactically valid email address.
    #[validate(email(message = "Ungültige E-Mail-Adresse"))]
    pub email: String,
    pub name: Option<String>,
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// When present, must be at least 5 characters (allows short codes like `"0176x"`).
    #[validate(length(min = 5, message = "Telefonnummer muss mindestens 5 Zeichen haben"))]
    pub phone: Option<String>,
}

/// Partial update applied to an existing customer record.
///
/// **Caller**: Admin API `PATCH /api/v1/customers/{id}` and the orchestrator
/// when new contact info arrives in a follow-up email.
/// **Why**: Using `Option` fields means callers only send the fields they want
/// to change; `None` fields are left unchanged in the database.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateCustomer {
    pub name: Option<String>,
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone: Option<String>,
}
