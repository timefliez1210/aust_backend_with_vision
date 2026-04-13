//! Generate real invoice PDFs for visual inspection.
//!
//! Run with: cargo test -p aust-offer-generator --test invoice_pdf_preview -- --nocapture
//!
//! Outputs 4 PDFs to /tmp/:
//!   1. rechnung_schilling_full.pdf    — full invoice with offer line items
//!   2. rechnung_schilling_full_negative.pdf — same + Gutschrift (negative line)
//!   3. rechnung_schilling_anzahlung.pdf    — partial first (30% Anzahlung)
//!   4. rechnung_schilling_restbetrag.pdf   — partial final (with Abzgl. deduction)

use aust_offer_generator::{generate_invoice_xlsx, InvoiceData, InvoiceLineItem, InvoiceType};
use chrono::NaiveDate;

/// Convert XLSX bytes to PDF using LibreOffice (like the main app does).
async fn xlsx_to_pdf(xlsx: &[u8], path: &str) -> Vec<u8> {
    match aust_offer_generator::convert_xlsx_to_pdf(xlsx).await {
        Ok(pdf) => {
            std::fs::write(path, &pdf).unwrap();
            pdf
        }
        Err(e) => {
            eprintln!("PDF conversion failed ({e}), saving as .xlsx instead");
            let xlsx_path = path.replace(".pdf", ".xlsx");
            std::fs::write(&xlsx_path, xlsx).unwrap();
            eprintln!("Saved as {xlsx_path} (open with LibreOffice to verify)");
            xlsx.to_vec()
        }
    }
}

/// Offer line items from Schilling inquiry (offer 2026-1107)
/// 6 Umzugshelfer, Fahrkostenpauschale, De/Montage, Halteverbotszone, Versicherung
fn schilling_offer_line_items() -> Vec<InvoiceLineItem> {
    vec![
        InvoiceLineItem {
            pos: 1,
            description: "6 Umzugshelfer".into(),
            quantity: 5.09,
            unit_price: 30.00,
            remark: None,
        },
        InvoiceLineItem {
            pos: 2,
            description: "Fahrkostenpauschale".into(),
            quantity: 1.0,
            unit_price: 24.13,
            remark: None,
        },
        InvoiceLineItem {
            pos: 3,
            description: "Demontage".into(),
            quantity: 1.0,
            unit_price: 50.00,
            remark: None,
        },
        InvoiceLineItem {
            pos: 4,
            description: "Montage".into(),
            quantity: 1.0,
            unit_price: 50.00,
            remark: None,
        },
        InvoiceLineItem {
            pos: 5,
            description: "Halteverbotszone".into(),
            quantity: 2.0,
            unit_price: 100.00,
            remark: Some("Beladestelle + Entladestelle".into()),
        },
        InvoiceLineItem {
            pos: 6,
            description: "Nürnbergerversicherung".into(),
            quantity: 1.0,
            unit_price: 0.00,
            remark: Some("Deckungssumme: 620,00 €/m³".into()),
        },
    ]
}

fn schilling_data(line_items: Vec<InvoiceLineItem>, invoice_type: InvoiceType) -> InvoiceData {
    InvoiceData {
        invoice_number: "2026-1107".into(),
        invoice_type,
        invoice_date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
        service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
        customer_name: "Schilling".into(),
        customer_email: "schilling@example.de".into(),
        company_name: None,
        attention_line: None,
        billing_street: "Schlickumerstr. 15A".into(),
        billing_city: "31157 Sarstedt".into(),
        offer_number: "2026-1107".into(),
        salutation: "Sehr geehrter Herr Schilling,".into(),
        line_items,
        #[allow(deprecated)]
        base_netto_cents: 0,
        #[allow(deprecated)]
        extra_services: vec![],
        #[allow(deprecated)]
        origin_street: String::new(),
        #[allow(deprecated)]
        origin_city: String::new(),
    }
}

#[tokio::test]
async fn preview_1_full_invoice_with_line_items() {
    let items = schilling_offer_line_items();
    let data = schilling_data(items, InvoiceType::Full);
    let xlsx = generate_invoice_xlsx(&data).expect("XLSX generation failed");

    // Save XLSX too for debugging
    std::fs::write("/tmp/rechnung_schilling_full.xlsx", &xlsx).unwrap();
    let _pdf = xlsx_to_pdf(&xlsx, "/tmp/rechnung_schilling_full.pdf").await;
    println!("✓ Full invoice → /tmp/rechnung_schilling_full.pdf");
    println!("  Nettosumme should be: (5.09×30) + 24.13 + 50 + 50 + (2×100) + 0 = 1,041.13 €");
}

#[tokio::test]
async fn preview_2_full_invoice_with_gutschrift() {
    let mut items = schilling_offer_line_items();
    // Add a credit/refund for a damaged item
    items.push(InvoiceLineItem {
        pos: 7,
        description: "Gutschrift: beschädigter Schrank".into(),
        quantity: 1.0,
        unit_price: -150.00,  // NEGATIVE — credit
        remark: None,
    });
    // Add an on-site extra
    items.push(InvoiceLineItem {
        pos: 8,
        description: "Klaviertransport".into(),
        quantity: 1.0,
        unit_price: 200.00,  // positive — extra service
        remark: Some("Vorab vereinbart".into()),
    });

    let data = schilling_data(items, InvoiceType::Full);
    let xlsx = generate_invoice_xlsx(&data).expect("XLSX generation failed");

    std::fs::write("/tmp/rechnung_schilling_full_negative.xlsx", &xlsx).unwrap();
    let _pdf = xlsx_to_pdf(&xlsx, "/tmp/rechnung_schilling_full_negative.pdf").await;
    println!("✓ Full invoice + Gutschrift → /tmp/rechnung_schilling_full_negative.pdf");
    println!("  Nettosumme should be: 1,041.13 - 150 + 200 = 1,091.13 €");
}

#[tokio::test]
async fn preview_3_partial_first_anzahlung() {
    // PartialFirst: single line item with 30% of total
    // Total netto from Schilling offer: 1,041.13, brutto = 1,041.13 * 1.19 = 1,238.94
    // 30% of brutto = 371.68, netto = 371.68 / 1.19 = 312.34
    let items = vec![InvoiceLineItem {
        pos: 1,
        description: "Anzahlung (30%) — gemäß Angebot Nr. 2026-1107".into(),
        quantity: 1.0,
        unit_price: 312.34,
        remark: None,
    }];

    let data = schilling_data(items, InvoiceType::PartialFirst { percent: 30 });
    let xlsx = generate_invoice_xlsx(&data).expect("XLSX generation failed");

    std::fs::write("/tmp/rechnung_schilling_anzahlung.xlsx", &xlsx).unwrap();
    let _pdf = xlsx_to_pdf(&xlsx, "/tmp/rechnung_schilling_anzahlung.pdf").await;
    println!("✓ Partial first (Anzahlung 30%) → /tmp/rechnung_schilling_anzahlung.pdf");
    println!("  Nettosumme should be: 312.34 €, Brutto: 371.68 €");
}

#[tokio::test]
async fn preview_4_partial_final_restbetrag() {
    // PartialFinal: offer line items + additional Klaviertransport + Abzgl. Anzahlung
    let mut items = schilling_offer_line_items();
    // On-site extra added on the day
    items.push(InvoiceLineItem {
        pos: 7,
        description: "Klaviertransport".into(),
        quantity: 1.0,
        unit_price: 200.00,
        remark: Some("Vorab vereinbart".into()),
    });
    // Credit
    items.push(InvoiceLineItem {
        pos: 8,
        description: "Gutschrift: beschädigter Schrank".into(),
        quantity: 1.0,
        unit_price: -150.00,
        remark: None,
    });
    // Deduction for Anzahlung (same amount as partial_first)
    items.push(InvoiceLineItem {
        pos: 9,
        description: "Abzgl. Anzahlung (30%)".into(),
        quantity: 1.0,
        unit_price: -312.34,  // negative — deducts what was already paid
        remark: None,
    });

    let data = schilling_data(items, InvoiceType::PartialFinal);
    let xlsx = generate_invoice_xlsx(&data).expect("XLSX generation failed");

    std::fs::write("/tmp/rechnung_schilling_restbetrag.xlsx", &xlsx).unwrap();
    let _pdf = xlsx_to_pdf(&xlsx, "/tmp/rechnung_schilling_restbetrag.pdf").await;
    println!("✓ Partial final (Restbetrag) → /tmp/rechnung_schilling_restbetrag.pdf");
    println!("  Nettosumme should be: 1,041.13 + 200 - 150 - 312.34 = 778.79 €");
}

#[tokio::test]
async fn preview_5_business_customer_invoice() {
    // Business customer with company name + attention line
    let items = vec![
        InvoiceLineItem {
            pos: 1,
            description: "2 Umzugshelfer".into(),
            quantity: 3.0,
            unit_price: 30.00,
            remark: None,
        },
        InvoiceLineItem {
            pos: 2,
            description: "3,5t Transporter".into(),
            quantity: 1.0,
            unit_price: 60.00,
            remark: Some("m. Koffer".into()),
        },
    ];

    let data = InvoiceData {
        invoice_number: "2026-1075".into(),
        invoice_type: InvoiceType::Full,
        invoice_date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
        service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 16).unwrap()),
        customer_name: "Karge".into(),
        customer_email: "karge@example.de".into(),
        company_name: Some("Steinberg GmbH".into()),
        attention_line: Some("z.Hd. Herrn Karge".into()),
        billing_street: "Bahnhofsplatz 3-4".into(),
        billing_city: "31134 Hildesheim".into(),
        offer_number: "2026-1075".into(),
        salutation: "Sehr geehrte Damen und Herren,".into(),
        line_items: items,
        #[allow(deprecated)]
        base_netto_cents: 0,
        #[allow(deprecated)]
        extra_services: vec![],
        #[allow(deprecated)]
        origin_street: String::new(),
        #[allow(deprecated)]
        origin_city: String::new(),
    };

    let xlsx = generate_invoice_xlsx(&data).expect("XLSX generation failed");
    std::fs::write("/tmp/rechnung_business_customer.xlsx", &xlsx).unwrap();
    let _pdf = xlsx_to_pdf(&xlsx, "/tmp/rechnung_business_customer.pdf").await;
    println!("✓ Business customer invoice → /tmp/rechnung_business_customer.pdf");
    println!("  Should show: A8=Steinberg GmbH, A9=z.Hd. Herrn Karge");
    println!("  Nettosumme: (3×30) + 60 = 150.00 €, Brutto: 178.50 €");
}