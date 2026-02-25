//! Quick preview test: generates XLSX → PDF → opens in viewer.
//! Run with: cargo test -p aust-offer-generator --test pdf_preview -- --nocapture

use aust_offer_generator::{generate_offer_xlsx, DetectedItemRow, OfferData, OfferLineItem};

fn make_item(name: &str, german: &str, re: f64, vol: f64, dims: Option<&str>) -> DetectedItemRow {
    DetectedItemRow {
        name: name.to_string(),
        volume_m3: vol,
        dimensions: dims.map(|s| s.to_string()),
        confidence: 0.9,
        german_name: Some(german.to_string()),
        re_value: Some(re),
        volume_source: Some("re_lookup".to_string()),
        crop_s3_key: None,
        bbox: None,
        bbox_image_index: None,
        source_image_urls: None,
    }
}

#[test]
fn preview_offer_pdf() {
    let data = OfferData {
        offer_number: "2026-TEST".to_string(),
        date: chrono::NaiveDate::from_ymd_opt(2026, 2, 24).unwrap(),
        valid_until: Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 10).unwrap()),
        customer_salutation: "Herrn".to_string(),
        customer_name: "Henning Rawohl".to_string(),
        customer_street: "Goslarsche Landstr. 6".to_string(),
        customer_city: "31135 Hildesheim".to_string(),
        customer_phone: "01707335168".to_string(),
        customer_email: "henning.rawohl@web.de".to_string(),
        greeting: "Sehr geehrter Herr Rawohl,".to_string(),
        moving_date: "16.03.2026".to_string(),
        origin_street: "Goslarsche Landstr. 6".to_string(),
        origin_city: "31135 Hildesheim".to_string(),
        origin_floor_info: "1. Stock".to_string(),
        dest_street: "Ostpreussenstr. 14a".to_string(),
        dest_city: "31191 Algermissen".to_string(),
        dest_floor_info: "1. Stock".to_string(),
        volume_m3: 11.3,
        persons: 4,
        estimated_hours: 2.0,
        rate_per_person_hour: 30.0,
        line_items: vec![
            OfferLineItem { description: "4 Umzugshelfer".to_string(), quantity: 4.0, unit_price: 30.0, is_labor: true, remark: None },
            OfferLineItem { description: "De/Montage".to_string(), quantity: 1.0, unit_price: 50.0, is_labor: false, remark: None },
            OfferLineItem { description: "Halteverbotszone".to_string(), quantity: 2.0, unit_price: 100.0, is_labor: false, remark: Some("Beladestelle + Entladestelle".to_string()) },
            OfferLineItem { description: "Umzugsmaterial".to_string(), quantity: 1.0, unit_price: 30.0, is_labor: false, remark: Some("inkl. Einpackservice".to_string()) },
            OfferLineItem { description: "3,5t Transporter".to_string(), quantity: 1.0, unit_price: 60.0, is_labor: false, remark: Some("m. Koffer".to_string()) },
            OfferLineItem { description: "Anfahrt/Abfahrt".to_string(), quantity: 1.0, unit_price: 51.29, is_labor: false, remark: None },
        ],
        detected_items: vec![
            make_item("Schreibtisch bis 1,6 m", "Schreibtisch", 12.0, 1.20, None),
            make_item("2x Sideboard groß", "Sideboard groß", 24.0, 2.40, Some("1.6 × 0.5 × 0.8 m")),
            make_item("Sofa, Couch, Liege je Sitz", "Sofa, Couch, Liege", 4.0, 0.40, None),
            make_item("Teppich", "Teppich", 3.0, 0.30, None),
            make_item("Tisch bis 0,6 m", "Tisch", 4.0, 0.40, None),
            make_item("TV-Schrank", "TV-Schrank", 4.0, 0.40, None),
            make_item("Französisches Bett komplett", "Französisches Bett", 15.0, 1.50, Some("1.4 × 2.0 × 0.5 m")),
            make_item("Nachttisch", "Nachttisch", 2.0, 0.20, None),
            make_item("Spiegel über 0,8 m", "Spiegel", 1.0, 0.10, None),
            make_item("Teewagen (nicht zerlegbar)", "Teewagen", 4.0, 0.40, None),
            make_item("Vitrine (Glasschrank)", "Vitrine", 10.0, 1.00, Some("1.0 × 0.5 × 1.8 m")),
            make_item("Kühlschrank / Truhe über 120 l", "Kühlschrank", 10.0, 1.00, None),
            make_item("2x Waschmaschine / Trockner", "Waschmaschine / Trockner", 10.0, 1.00, None),
            make_item("Aktenschrank je angefangener m", "Aktenschrank", 8.0, 0.80, Some("0.8 × 0.4 × 1.8 m")),
            make_item("Stehlampe", "Stehlampe", 2.0, 0.20, None),
        ],
    };

    // Generate XLSX
    let xlsx = generate_offer_xlsx(&data).expect("generate xlsx");

    std::fs::write("/tmp/offer_preview.xlsx", &xlsx).expect("write xlsx");
    println!("XLSX saved to /tmp/offer_preview.xlsx");

    // Convert to PDF via LibreOffice
    let output = std::process::Command::new("libreoffice")
        .args([
            "--headless", "--calc", "--convert-to", "pdf",
            "--outdir", "/tmp/", "/tmp/offer_preview.xlsx",
        ])
        .output()
        .expect("libreoffice");

    if !output.status.success() {
        panic!("LibreOffice failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let pdf_path = "/tmp/offer_preview.pdf";
    assert!(std::path::Path::new(pdf_path).exists(), "PDF not generated");

    let metadata = std::fs::metadata(pdf_path).unwrap();
    println!("PDF saved to {pdf_path} ({} bytes)", metadata.len());

    let _ = std::process::Command::new("xdg-open").arg(pdf_path).spawn();
}
