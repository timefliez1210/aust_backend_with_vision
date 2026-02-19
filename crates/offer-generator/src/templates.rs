use aust_core::models::{Address, Customer, Offer, PricingResult, Quote};

pub struct OfferTemplate {
    company_name: String,
    company_address: String,
}

impl OfferTemplate {
    pub fn new(company_name: String, company_address: String) -> Self {
        Self {
            company_name,
            company_address,
        }
    }

    pub fn render(
        &self,
        customer: &Customer,
        quote: &Quote,
        offer: &Offer,
        pricing: &PricingResult,
        origin_address: Option<&Address>,
        destination_address: Option<&Address>,
    ) -> String {
        let price_eur = offer.price_cents as f64 / 100.0;

        format!(
            r#"
ANGEBOT

Von: {}
{}

An: {}
{}

Datum: {}

---

Sehr geehrte/r {},

vielen Dank für Ihre Anfrage. Wir unterbreiten Ihnen folgendes Angebot:

UMZUGSDETAILS
=============
Von: {}
Nach: {}

Geschätztes Volumen: {:.1} m³
Entfernung: {:.1} km

PREISAUFSTELLUNG
================
Arbeitskosten ({} Helfer, {:.0} Stunden): {:.2} €
Kilometerkosten: {:.2} €
Stockwerkzuschlag: {:.2} €
Terminzuschlag: {:.2} €

GESAMTPREIS: {:.2} €

Dieses Angebot ist gültig bis: {}

Mit freundlichen Grüßen,
{}
"#,
            self.company_name,
            self.company_address,
            customer.name.as_deref().unwrap_or(&customer.email),
            customer.email,
            chrono::Utc::now().format("%d.%m.%Y"),
            customer.name.as_deref().unwrap_or("Kunde/Kundin"),
            origin_address
                .map(|a| format!(
                    "{}, {} {}",
                    a.street,
                    a.postal_code.as_deref().unwrap_or(""),
                    a.city
                ))
                .unwrap_or_else(|| "Nicht angegeben".to_string()),
            destination_address
                .map(|a| format!(
                    "{}, {} {}",
                    a.street,
                    a.postal_code.as_deref().unwrap_or(""),
                    a.city
                ))
                .unwrap_or_else(|| "Nicht angegeben".to_string()),
            quote.estimated_volume_m3.unwrap_or(0.0),
            quote.distance_km.unwrap_or(0.0),
            pricing.estimated_helpers,
            pricing.estimated_hours,
            pricing.breakdown.base_labor_cents as f64 / 100.0,
            pricing.breakdown.distance_cents as f64 / 100.0,
            pricing.breakdown.floor_surcharge_cents as f64 / 100.0,
            pricing.breakdown.date_adjustment_cents as f64 / 100.0,
            price_eur,
            offer
                .valid_until
                .map(|d| d.format("%d.%m.%Y").to_string())
                .unwrap_or_else(|| "Unbegrenzt".to_string()),
            self.company_name,
        )
    }
}
// hier muss der feeedback loop initialisiert werden
// fahrtweg is egal fuer den vergleich
// anfahrt/abfahrt sollte seperat analysiert werden (brainstorm)
