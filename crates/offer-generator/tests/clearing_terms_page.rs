//! The clearing-job terms page, end to end.
//!
//! Entrümpelung and Haushaltsauflösung must not ship the Umzug conditions
//! (Kartons max. 20 kg, Designermöbel, Tragewege). After the PDF is converted,
//! page 2 is swapped for the static clearing page — see `substitute_clearing_page_2`.
//! The fixture is offer 2026-0323 (Haushaltsauflösung, Pia Morgenroth), which
//! went out with the wrong page 2 before the fix reached production.
//!
//! Needs LibreOffice and poppler-utils, so it stays behind `--ignored`:
//!   cargo test -p aust-offer-generator --test clearing_terms_page -- --ignored --nocapture

use aust_offer_generator::{
    convert_xlsx_to_pdf, generate_offer_xlsx, substitute_clearing_page_2, OfferData, OfferLineItem,
};

/// Wording unique to the clearing terms page.
const CLEARING_ONLY: &str = "abwegig der Bestandsaufnahme (5m³)";
/// Wording unique to the template's Umzug terms page.
const UMZUG_ONLY: &[&str] = &["Kartons dürfen max", "Designermöbel"];

fn item(description: &str, quantity: f64, unit_price: f64, is_labor: bool, remark: Option<&str>, flat_total: Option<f64>) -> OfferLineItem {
    OfferLineItem {
        description: description.to_string(),
        quantity,
        unit_price,
        is_labor,
        remark: remark.map(String::from),
        flat_total,
    }
}

/// Offer 2026-0323 as stored in production.
fn offer_2026_0323() -> OfferData {
    OfferData {
        offer_number: "2026-0323".to_string(),
        date: chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
        valid_until: None,
        customer_salutation: "Frau".to_string(),
        customer_name: "Pia Morgenroth".to_string(),
        customer_street: "Bergstr 51C".to_string(),
        customer_city: "31137 Hildesheim".to_string(),
        customer_phone: "0175-6077561".to_string(),
        customer_email: Some("morgenroth@gbroi.com".to_string()),
        company_name: None,
        attention_line: None,
        greeting: "Sehr geehrte Frau Morgenroth,".to_string(),
        // No scheduled date on the inquiry.
        moving_date: "nach Vereinbarung".to_string(),
        origin_street: "Bergstr 51C".to_string(),
        origin_city: "31137 Hildesheim".to_string(),
        origin_floor_info: "1. OG".to_string(),
        // A clearing job has no destination.
        dest_street: String::new(),
        dest_city: String::new(),
        dest_floor_info: String::new(),
        stop_street: String::new(),
        stop_city: String::new(),
        stop_floor_info: String::new(),
        volume_m3: 0.0,
        persons: 6,
        estimated_hours: 8.0,
        rate_per_person_hour: 30.0,
        line_items: vec![
            item("6 Umzugshelfer", 8.0, 30.0, true, None, None),
            item("3,5t Transporter m. Koffer", 2.0, 80.0, false, None, None),
            item("Fahrkostenpauschale", 0.0, 0.0, false, None, Some(80.0)),
            item(
                "Entsorgung gemischter Abfälle ",
                2.0,
                160.0,
                false,
                Some("Abrechnung nach tatsächlichem Gewicht, je Tonne"),
                None,
            ),
            item(
                "Entsorgungs Holz",
                1.0,
                70.0,
                false,
                Some("Abrechnung nach tatsächlichem Gewicht, je Tonne"),
                None,
            ),
        ],
        detected_items: Vec::new(),
        headline_override: Some("Haushaltsauflösung 50 m³".to_string()),
    }
}

fn page_text(pdf: &std::path::Path, page: u32) -> String {
    let out = std::process::Command::new("pdftotext")
        .args(["-layout", "-f", &page.to_string(), "-l", &page.to_string()])
        .arg(pdf)
        .arg("-")
        .output()
        .expect("pdftotext");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
#[ignore = "requires LibreOffice + poppler-utils; run with --ignored"]
async fn haushaltsaufloesung_gets_the_clearing_terms_page() {
    let xlsx = generate_offer_xlsx(&offer_2026_0323()).expect("generate xlsx");
    std::fs::write("/tmp/kva_2026-0323.xlsx", &xlsx).expect("write xlsx");
    let pdf = convert_xlsx_to_pdf(&xlsx).await.expect("convert to pdf");
    let swapped = substitute_clearing_page_2(&pdf)
        .await
        .expect("substitute the clearing terms page");

    let path = std::path::Path::new("/tmp/kva_2026-0323.pdf");
    std::fs::write(path, &swapped).expect("write pdf");
    println!("PDF written to {}", path.display());

    let page_1 = page_text(path, 1);
    assert!(page_1.contains("2026-0323"), "page 1 lost the offer number");
    assert!(
        page_1.contains("Haushaltsauflösung 50 m³"),
        "page 1 lost the headline"
    );

    let page_2 = page_text(path, 2);
    assert!(
        page_2.contains(CLEARING_ONLY),
        "page 2 is not the clearing terms page"
    );
    for needle in UMZUG_ONLY {
        assert!(
            !page_2.contains(needle),
            "page 2 still carries the Umzug condition {needle:?}"
        );
    }
    // A signature rule that fits reads "____   ____" on one line; a wrapped one
    // leaves a line of nothing but underscores behind.
    let wrapped: Vec<&str> = page_2
        .lines()
        .filter(|l| !l.trim().is_empty() && l.trim().chars().all(|c| c == '_'))
        .collect();
    assert!(wrapped.is_empty(), "signature rule wrapped: {wrapped:?}");
}
