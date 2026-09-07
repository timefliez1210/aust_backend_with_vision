//! The Zwischenstopp block on the KVA.
//!
//! A move can have an intermediate stop (storage, second pickup). It reached the
//! pricing and the route calculation but never the customer's KVA, because
//! `OfferData` carried no stop fields at all. It is now printed in column C,
//! between the Beladestelle (A/B) and the Entladestelle (F/G).
//!
//! The PDF preview needs LibreOffice and stays behind `--ignored`:
//!   cargo test -p aust-offer-generator --test offer_zwischenstopp -- --ignored --nocapture

use aust_offer_generator::{generate_offer_xlsx, OfferData, OfferLineItem};
use std::io::Read;

fn base_data() -> OfferData {
    OfferData {
        offer_number: "2026-0320".to_string(),
        date: chrono::NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(),
        valid_until: None,
        customer_salutation: "Herrn".to_string(),
        customer_name: "Martin Broistedt".to_string(),
        customer_street: "Breslauerstr. 10".to_string(),
        customer_city: "31137 Hildesheim".to_string(),
        customer_phone: "05121 123456".to_string(),
        customer_email: Some("martinbroistedt@googlemail.com".to_string()),
        company_name: None,
        attention_line: None,
        greeting: "Sehr geehrter Herr Broistedt,".to_string(),
        moving_date: "24.09.2026".to_string(),
        origin_street: "Breslauerstr. 10".to_string(),
        origin_city: "31137 Hildesheim".to_string(),
        origin_floor_info: "Erdgeschoss".to_string(),
        dest_street: "Moritzstr. 22".to_string(),
        dest_city: "31137 Hildesheim".to_string(),
        dest_floor_info: "3. OG".to_string(),
        stop_street: String::new(),
        stop_city: String::new(),
        stop_floor_info: String::new(),
        volume_m3: 32.0,
        persons: 4,
        estimated_hours: 8.0,
        rate_per_person_hour: 30.0,
        line_items: vec![
            OfferLineItem {
                description: "4 Umzugshelfer".to_string(),
                quantity: 8.0,
                unit_price: 30.0,
                is_labor: true,
                remark: None,
                flat_total: None,
            },
            OfferLineItem {
                description: "Halteverbotszone".to_string(),
                quantity: 2.0,
                unit_price: 100.0,
                is_labor: false,
                remark: Some("Beladestelle + Entladestelle".to_string()),
                flat_total: None,
            },
        ],
        detected_items: Vec::new(),
        headline_override: None,
    }
}

fn with_stop() -> OfferData {
    OfferData {
        stop_street: "Lagerstr. 4".to_string(),
        stop_city: "31139 Hildesheim".to_string(),
        stop_floor_info: "Halle 2".to_string(),
        ..base_data()
    }
}

/// Read `xl/worksheets/sheet1.xml` back out of the generated workbook.
fn sheet1(xlsx: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(xlsx)).expect("open xlsx");
    let mut file = zip
        .by_name("xl/worksheets/sheet1.xml")
        .expect("sheet1.xml in workbook");
    let mut out = String::new();
    file.read_to_string(&mut out).expect("read sheet1.xml");
    out
}

#[test]
fn stop_address_is_written_between_the_two_blocks() {
    let xlsx = generate_offer_xlsx(&with_stop()).expect("generate xlsx");
    let sheet = sheet1(&xlsx);

    assert!(
        sheet.contains("Zwischenstopp:"),
        "the C25 label is missing from the sheet"
    );
    for line in ["Lagerstr. 4", "31139 Hildesheim", "Halle 2"] {
        assert!(sheet.contains(line), "stop line {line:?} is missing");
    }
    // The three other blocks must survive untouched.
    assert!(sheet.contains("Breslauerstr. 10"), "origin lost");
    assert!(sheet.contains("Moritzstr. 22"), "destination lost");
}

#[test]
fn without_a_stop_the_label_stays_off_the_page() {
    let xlsx = generate_offer_xlsx(&base_data()).expect("generate xlsx");
    let sheet = sheet1(&xlsx);

    assert!(
        !sheet.contains("Zwischenstopp"),
        "a move without a stop must not print the label"
    );
}

/// Visual preview only — writes both variants next to each other.
#[test]
#[ignore = "requires LibreOffice; run with --ignored to regenerate the preview PDFs"]
fn preview_zwischenstopp_pdf() {
    for (name, data) in [("mit_stopp", with_stop()), ("ohne_stopp", base_data())] {
        let xlsx = generate_offer_xlsx(&data).expect("generate xlsx");
        let path = format!("/tmp/kva_{name}.xlsx");
        std::fs::write(&path, &xlsx).expect("write xlsx");

        let out = std::process::Command::new("libreoffice")
            .args(["--headless", "--calc", "--convert-to", "pdf", "--outdir", "/tmp/", &path])
            .output()
            .expect("libreoffice");
        assert!(out.status.success(), "LibreOffice failed for {name}");
        println!("PDF written to /tmp/kva_{name}.pdf");
    }
}
