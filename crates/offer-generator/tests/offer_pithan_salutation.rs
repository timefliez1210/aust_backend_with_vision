//! Regression preview for inquiry 019fb30a-e012-7e51-aee3-e0496d82eac7 (Lilian Pithan).
//!
//! The customer is stored with `salutation = "Frau"` and `name = "Lilian Pithan"`
//! but no structured `first_name`/`last_name`. The address block rendered "Frau"
//! correctly while the letter greeting fell through to the female-first-name
//! heuristic — "Lilian" is not in that table — and produced
//! "Sehr geehrter Herr Pithan,".
//!
//! Run the PDF preview (needs LibreOffice, writes into the temp dir) with:
//!   cargo test -p aust-offer-generator --test offer_pithan_salutation -- --ignored --nocapture

use aust_core::models::{resolve_address_salutation, resolve_greeting};
use aust_offer_generator::{generate_offer_xlsx, OfferData, OfferLineItem};

/// Exact customer row as returned by the agents API for the live inquiry.
const SALUTATION: Option<&str> = Some("Frau");
const FIRST_NAME: Option<&str> = None;
const LAST_NAME: Option<&str> = None;
const NAME: Option<&str> = Some("Lilian Pithan");

#[test]
fn greeting_uses_stored_salutation_not_first_name_heuristic() {
    let greeting = resolve_greeting(SALUTATION, FIRST_NAME, LAST_NAME, NAME);
    assert_eq!(greeting, "Sehr geehrte Frau Pithan,");
    assert_eq!(resolve_address_salutation(SALUTATION, NAME), "Frau");
}

/// Visual preview only — requires a LibreOffice binary, so it stays out of the
/// default run. The greeting itself is asserted above without any I/O.
#[test]
#[ignore = "requires LibreOffice; run with --ignored to regenerate the preview PDF"]
fn preview_pithan_offer_pdf() {
    let data = OfferData {
        offer_number: "2026-0257".to_string(),
        date: chrono::NaiveDate::from_ymd_opt(2026, 7, 30).unwrap(),
        valid_until: Some(chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap()),
        customer_salutation: resolve_address_salutation(SALUTATION, NAME),
        customer_name: "Lilian Pithan".to_string(),
        customer_street: "Sarlinenstr. 8".to_string(),
        customer_city: "31162 Bad Salzdetfurth".to_string(),
        customer_phone: "015757910623".to_string(),
        customer_email: Some("lilianmaria.pithan@gmail.com".to_string()),
        company_name: None,
        attention_line: None,
        greeting: resolve_greeting(SALUTATION, FIRST_NAME, LAST_NAME, NAME),
        moving_date: "08.08.2026".to_string(),
        origin_street: "Sarlinenstr. 8".to_string(),
        origin_city: "31162 Bad Salzdetfurth".to_string(),
        origin_floor_info: String::new(),
        dest_street: String::new(),
        dest_city: String::new(),
        dest_floor_info: String::new(),
        volume_m3: 0.0,
        persons: 3,
        estimated_hours: 3.0,
        rate_per_person_hour: 28.0,
        line_items: vec![
            OfferLineItem {
                description: "3 Umzugshelfer".to_string(),
                quantity: 3.0,
                unit_price: 28.0,
                is_labor: true,
                remark: None,
                flat_total: None,
            },
            OfferLineItem {
                description: "Fahrkostenpauschale".to_string(),
                quantity: 0.0,
                unit_price: 0.0,
                is_labor: false,
                remark: None,
                flat_total: Some(100.0),
            },
            OfferLineItem {
                description: "Nürnbergerversicherung".to_string(),
                quantity: 1.0,
                unit_price: 0.0,
                is_labor: false,
                remark: Some("Deckungssumme: 620,00 Euro / m³".to_string()),
                flat_total: Some(0.0),
            },
        ],
        detected_items: vec![],
        headline_override: None,
    };

    assert_eq!(data.greeting, "Sehr geehrte Frau Pithan,");

    let out_dir = std::env::temp_dir();
    let out = out_dir.display().to_string();
    let xlsx_path = format!("{out}/angebot_2026-0257_pithan.xlsx");
    let pdf_path = format!("{out}/angebot_2026-0257_pithan.pdf");

    let xlsx = generate_offer_xlsx(&data).expect("generate xlsx");
    std::fs::write(&xlsx_path, &xlsx).expect("write xlsx");
    println!("XLSX saved to {xlsx_path}");

    let output = std::process::Command::new("libreoffice")
        .args([
            "--headless",
            "--calc",
            "--convert-to",
            "pdf",
            "--outdir",
            &out,
            &xlsx_path,
        ])
        .output()
        .expect("libreoffice");

    if !output.status.success() {
        panic!(
            "LibreOffice failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(std::path::Path::new(&pdf_path).exists(), "PDF not generated");
    println!(
        "PDF saved to {pdf_path} ({} bytes)",
        std::fs::metadata(&pdf_path).unwrap().len()
    );
}
