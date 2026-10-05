//! Which document template a tenant's offers, invoices and forms are built on.
//!
//! Aust's templates are compiled in (`templates/*`) and carry Aust's letterhead,
//! bank footer and logo. Every other tenant brings its own, loaded at startup from
//! `tenant_templates` ([`register`]). A tenant without one gets an error — never
//! Aust's letterhead. See `docs/MULTI_TENANT.md`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use aust_core::tenant::{self, TenantId, AUST};

use crate::OfferError;

/// A template a tenant can replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemplateKind {
    /// Kostenvoranschlag (`offer_template.xlsx`).
    Offer,
    /// Rechnung (`Rechnung_Vorlage_v4.xlsx`).
    Invoice,
    /// Reisekostenabrechnung (`reisekosten_template.xlsx`).
    TravelExpense,
    /// Page 2 of a clearing (Entrümpelung) KVA (`entruempelung_kva_seite2.pdf`).
    ClearingPage2,
}

impl TemplateKind {
    /// Name stored in `tenant_templates.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offer => "offer",
            Self::Invoice => "invoice",
            Self::TravelExpense => "travel_expense",
            Self::ClearingPage2 => "clearing_page_2",
        }
    }

    /// Parse a `tenant_templates.kind`.
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Offer, Self::Invoice, Self::TravelExpense, Self::ClearingPage2]
            .into_iter()
            .find(|k| k.as_str() == s)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Offer => "Angebotsvorlage",
            Self::Invoice => "Rechnungsvorlage",
            Self::TravelExpense => "Reisekostenvorlage",
            Self::ClearingPage2 => "Entrümpelungs-Seite 2",
        }
    }
}

type Registry = HashMap<(TenantId, TemplateKind), Arc<[u8]>>;

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(Default::default);

/// Make `bytes` the template of `kind` for `tenant` (startup, and after an upload).
pub fn register(tenant: TenantId, kind: TemplateKind, bytes: Vec<u8>) {
    REGISTRY
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert((tenant, kind), bytes.into());
}

/// The template of `kind` for the running tenant: Aust (and code outside any
/// tenant scope) gets the compiled-in `builtin`; any other tenant its registered
/// one, or an error.
pub(crate) fn for_current(kind: TemplateKind, builtin: &'static [u8]) -> Result<Arc<[u8]>, OfferError> {
    let t = match tenant::current() {
        None => return Ok(Arc::from(builtin)),
        Some(t) if t == AUST => return Ok(Arc::from(builtin)),
        Some(t) => t,
    };
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(t, kind))
        .cloned()
        .ok_or_else(|| {
            OfferError::Template(format!("Für diese Firma ist keine {} hinterlegt.", kind.label()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTIN: &[u8] = b"aust";

    #[tokio::test]
    async fn aust_gets_the_builtin_and_others_their_own_or_an_error() {
        assert_eq!(&*for_current(TemplateKind::Offer, BUILTIN).unwrap(), BUILTIN);
        assert_eq!(
            &*tenant::scope(AUST, async { for_current(TemplateKind::Offer, BUILTIN) }).await.unwrap(),
            BUILTIN
        );

        let other = TenantId(uuid::Uuid::now_v7());
        let missing = tenant::scope(other, async { for_current(TemplateKind::Invoice, BUILTIN) }).await;
        assert!(missing.is_err(), "another company must never get Aust's letterhead");

        register(other, TemplateKind::Invoice, b"zweite".to_vec());
        let own = tenant::scope(other, async { for_current(TemplateKind::Invoice, BUILTIN) }).await;
        assert_eq!(&*own.unwrap(), b"zweite");
    }

    #[test]
    fn kinds_round_trip() {
        for k in [TemplateKind::Offer, TemplateKind::Invoice, TemplateKind::TravelExpense, TemplateKind::ClearingPage2] {
            assert_eq!(TemplateKind::parse(k.as_str()), Some(k));
        }
    }
}
