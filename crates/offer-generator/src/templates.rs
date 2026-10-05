//! Which document template a tenant's offers, invoices and forms are built on.
//!
//! Aust's templates are compiled in (`templates/*`) and carry Aust's letterhead,
//! bank footer and logo. Every other tenant uses, in this order:
//! 1. a template it uploaded (`tenant_templates`, [`register`]);
//! 2. Aust's template with its own letterhead and logo swapped in
//!    ([`register_letterhead`], see `crate::letterhead`);
//! 3. otherwise an error — never Aust's letterhead. See `docs/MULTI_TENANT.md`.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use aust_core::tenant::{self, TenantId, AUST};

use crate::letterhead::{self, Letterhead};
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
static LETTERHEADS: LazyLock<RwLock<HashMap<TenantId, Arc<Letterhead>>>> = LazyLock::new(Default::default);
/// Templates derived from a letterhead, built on first use.
static DERIVED: LazyLock<RwLock<Registry>> = LazyLock::new(Default::default);

/// Make `letterhead` the source of `tenant`'s derived templates (startup, and
/// after the company's details or logo change).
pub fn register_letterhead(tenant: TenantId, letterhead: Letterhead) {
    LETTERHEADS.write().unwrap_or_else(|e| e.into_inner()).insert(tenant, Arc::new(letterhead));
    DERIVED.write().unwrap_or_else(|e| e.into_inner()).retain(|(t, _), _| *t != tenant);
}

/// Make `bytes` the template of `kind` for `tenant` (startup, and after an upload).
pub fn register(tenant: TenantId, kind: TemplateKind, bytes: Vec<u8>) {
    REGISTRY
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert((tenant, kind), bytes.into());
}

/// The running tenant's template of `kind`, as the generators would use it —
/// for checks (does this company have working documents?) and downloads.
pub fn current_bytes(kind: TemplateKind) -> Result<Arc<[u8]>, OfferError> {
    let builtin: &'static [u8] = match kind {
        TemplateKind::Offer => include_bytes!("../../../templates/offer_template.xlsx"),
        TemplateKind::Invoice => include_bytes!("../../../templates/Rechnung_Vorlage_v4.xlsx"),
        TemplateKind::TravelExpense => include_bytes!("../../../templates/reisekosten_template.xlsx"),
        TemplateKind::ClearingPage2 => include_bytes!("../../../templates/entruempelung_kva_seite2.pdf"),
    };
    for_current(kind, builtin)
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
    if let Some(own) = REGISTRY.read().unwrap_or_else(|e| e.into_inner()).get(&(t, kind)) {
        return Ok(own.clone());
    }
    if let Some(done) = DERIVED.read().unwrap_or_else(|e| e.into_inner()).get(&(t, kind)) {
        return Ok(done.clone());
    }
    let lh = LETTERHEADS.read().unwrap_or_else(|e| e.into_inner()).get(&t).cloned();
    // The clearing page is a finished PDF — there is nothing to derive it from.
    let Some(lh) = lh.filter(|_| kind != TemplateKind::ClearingPage2) else {
        return Err(OfferError::Template(format!(
            "Für diese Firma ist keine {} hinterlegt.",
            kind.label()
        )));
    };
    let derived: Arc<[u8]> = letterhead::derive(builtin, &lh)?.into();
    DERIVED
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert((t, kind), derived.clone());
    Ok(derived)
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

    /// A company with a letterhead but no uploads gets Aust's templates with its
    /// own details; an upload still wins; the clearing page cannot be derived.
    #[tokio::test]
    async fn a_letterhead_derives_working_templates() {
        const OFFER: &[u8] = include_bytes!("../../../templates/offer_template.xlsx");
        let other = TenantId(uuid::Uuid::now_v7());
        register_letterhead(other, crate::letterhead::tests::zweite());
        let derived = tenant::scope(other, async { for_current(TemplateKind::Offer, OFFER) }).await.unwrap();
        assert_ne!(&*derived, OFFER);
        assert!(tenant::scope(other, async { for_current(TemplateKind::ClearingPage2, BUILTIN) }).await.is_err());

        register(other, TemplateKind::Offer, b"eigene".to_vec());
        let own = tenant::scope(other, async { for_current(TemplateKind::Offer, OFFER) }).await.unwrap();
        assert_eq!(&*own, b"eigene");
    }

    #[test]
    fn kinds_round_trip() {
        for k in [TemplateKind::Offer, TemplateKind::Invoice, TemplateKind::TravelExpense, TemplateKind::ClearingPage2] {
            assert_eq!(TemplateKind::parse(k.as_str()), Some(k));
        }
    }
}
