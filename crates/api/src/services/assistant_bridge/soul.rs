//! Josie's persona for the running tenant (docs/MULTI_TENANT.md, step 4).

use std::sync::Arc;

use aust_assistant::Soul;
use aust_core::tenant::{self, AUST};

use crate::AppState;

/// Aust (and code outside a tenant scope) keeps `SOUL.md` as loaded at startup.
/// Any other company uses its own `tenants.soul_md`, or — without one, or if it
/// does not parse — a neutral persona under its own name, never Aust's.
pub(crate) async fn for_current(state: &AppState) -> Arc<Soul> {
    match tenant::current() {
        None => return state.soul.clone(),
        Some(t) if t == AUST => return state.soul.clone(),
        Some(_) => {}
    }
    let row: Result<(String, Option<String>), sqlx::Error> =
        sqlx::query_as("SELECT name, soul_md FROM tenants WHERE id = current_tenant_id()")
            .fetch_one(&state.db)
            .await;
    let (name, soul_md) = match row {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Tenant soul not loaded: {e}");
            (String::from("die Firma"), None)
        }
    };
    if let Some(md) = soul_md {
        match aust_assistant::soul::parse(&md) {
            Ok(soul) => return Arc::new(soul),
            Err(e) => tracing::warn!("tenants.soul_md does not parse ({e}); using the neutral persona"),
        }
    }
    Arc::new(neutral(&name))
}

/// The persona of a company that has not written its own.
fn neutral(company: &str) -> Soul {
    Soul {
        persona: format!(
            "Du bist die digitale Büroassistenz von {company}. Du hilfst dem Inhaber und \
             dem Team im täglichen Betrieb eines Umzugsunternehmens."
        ),
        hard_rules: "Erfinde keine Daten. Handle nur über die bereitgestellten Werkzeuge.".into(),
        domain_primer: "Kernprozess: Anfrage → Schätzung → Angebot → Terminplanung → Rechnung → Zahlung.".into(),
        tone: "Sachlich, knapp, auf Deutsch, Sie-Form gegenüber Kunden.".into(),
        escalation: "Bei Unsicherheit erst nachfragen.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_neutral_persona_names_the_company_and_not_aust() {
        let soul = neutral("Zweite Umzüge GmbH");
        assert!(soul.persona.contains("Zweite Umzüge GmbH"));
        let all = format!("{soul:?}");
        assert!(!all.contains("Aust") && !all.contains("Josie") && !all.contains("Alex"));
    }
}
