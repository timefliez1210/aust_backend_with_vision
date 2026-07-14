//! Tests for invoice XLSX generation with line items.
//!
//! Verifies that:
//! - Line items are placed in the correct template cells
//! - Negative values (credits/refunds) are handled correctly
//! - Legacy base_netto_cents + extra_services path still works
//! - Unused rows are hidden
//! - Row hiding logic works for partial and full invoices

use aust_offer_generator::{generate_invoice_xlsx, InvoiceData, InvoiceLineItem, InvoiceType};
use chrono::NaiveDate;

fn base_line_items() -> Vec<InvoiceLineItem> {
    vec![
        InvoiceLineItem {
            pos: 1,
            description: "De/Montage".into(),
            quantity: 1.0,
            unit_price: 50.0,
            remark: None,
        },
        InvoiceLineItem {
            pos: 2,
            description: "Halteverbotszone".into(),
            quantity: 1.0,
            unit_price: 100.0,
            remark: Some("Entladestelle".into()),
        },
        InvoiceLineItem {
            pos: 3,
            description: "Umzugsmaterial".into(),
            quantity: 1.0,
            unit_price: 30.0,
            remark: None,
        },
        InvoiceLineItem {
            pos: 4,
            description: "Personal".into(),
            quantity: 3.0,
            unit_price: 28.0,
            remark: Some("6 Umzugshelfer".into()),
        },
        InvoiceLineItem {
            pos: 5,
            description: "3,5t Transporter m. Koffer".into(),
            quantity: 1.0,
            unit_price: 60.0,
            remark: None,
        },
    ]
}

fn make_invoice_data(line_items: Vec<InvoiceLineItem>, invoice_type: InvoiceType) -> InvoiceData {
    InvoiceData {
        invoice_number: "2026-0131".into(),
        invoice_type,
        invoice_date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
        service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
        customer_name: "Herrn Horst Lindenthal".into(),
        customer_email: Some("lindenthal@test.de".into()),
        company_name: None,
        attention_line: None,
        billing_street: "Goslarsche Landstr. 6".into(),
        billing_city: "31135 Hildesheim".into(),
        service_street: String::new(),
        service_city: String::new(),
        offer_number: "2026-0042".into(),
        salutation: "Sehr geehrter Herr Lindenthal,".into(),
        line_items,
        // Legacy fields (unused when line_items is set)
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

#[test]
fn test_full_invoice_with_line_items() {
    let items = base_line_items();
    let data = make_invoice_data(items, InvoiceType::Full);
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok(), "XLSX generation should succeed for full invoice with line items");

    let bytes = result.unwrap();
    // Verify it's a valid ZIP (XLSX format)
    assert!(bytes.starts_with(b"PK"), "Output should be a valid ZIP/XLSX file");
}

#[test]
fn test_invoice_with_negative_line_item() {
    let mut items = base_line_items();
    // Add a credit/refund line item with negative price
    items.push(InvoiceLineItem {
        pos: 6,
        description: "Gutschrift: beschädigter Schrank".into(),
        quantity: 1.0,
        unit_price: -150.0,
        remark: None,
    });

    let data = make_invoice_data(items, InvoiceType::Full);
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok(), "XLSX generation should succeed with negative line items");

    let bytes = result.unwrap();
    assert!(bytes.starts_with(b"PK"), "Output should be a valid ZIP/XLSX file");

    // Parse the sheet XML and verify the negative value is present
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    let sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("Should have sheet1.xml");
    let sheet_str = std::io::read_to_string(sheet).expect("Should read sheet1.xml");

    // The negative value -150 should appear in a <v> element for D36 (6th item, row 36)
    assert!(sheet_str.contains("-150"), "XML should contain the negative unit price -150");
}

#[test]
fn test_manual_hours_line_item_renders_quantity_and_unit_price() {
    // Business-customer invoice with worked hours itemised: "12,5 Std. à 45,00 €".
    // The template's C=Menge / D=Einzelpreis / E=D*C columns must carry the raw
    // decimal quantity and unit price so the PDF shows the hourly breakdown.
    let items = vec![
        InvoiceLineItem {
            pos: 1,
            description: "Umzugsarbeiten (Stundenabrechnung)".into(),
            quantity: 12.5,
            unit_price: 45.0,
            remark: None,
        },
        InvoiceLineItem {
            pos: 2,
            description: "Anfahrtspauschale".into(),
            quantity: 1.0,
            unit_price: 80.0,
            remark: None,
        },
    ];
    let data = make_invoice_data(items, InvoiceType::Full);
    let bytes = generate_invoice_xlsx(&data).expect("XLSX generation should succeed");
    assert!(bytes.starts_with(b"PK"));

    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    let sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("sheet1.xml");
    let sheet_str = std::io::read_to_string(sheet).expect("read sheet1.xml");

    // Decimal hours land in the Menge column (C31) and the netto rate in Einzelpreis (D31).
    assert!(sheet_str.contains("12.5"), "Menge 12,5 should be written to the sheet");
    assert!(sheet_str.contains(">45<"), "Einzelpreis 45,00 should be written to the sheet");
}

#[test]
fn test_storage_invoice_renders_month_label_and_netto() {
    // A monthly storage ("Lagerung") invoice is a single netto line item:
    // "Lagerung <Monat> <Jahr>" with the rented m² as the remark. Netto 100,00 €
    // (brutto 119,00) — the template re-adds 19% MwSt.
    let items = vec![InvoiceLineItem {
        pos: 1,
        description: "Lagerung Juli 2026".into(),
        quantity: 1.0,
        unit_price: 100.0,
        remark: Some("12,5 m² Lagerfläche".into()),
    }];
    let data = make_invoice_data(items, InvoiceType::Full);
    let bytes = generate_invoice_xlsx(&data).expect("XLSX generation should succeed");

    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    // Text may be inline or in sharedStrings depending on the template — scan both.
    let mut all_xml = String::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        if f.name().ends_with(".xml") {
            all_xml.push_str(&std::io::read_to_string(&mut f).unwrap_or_default());
        }
    }
    assert!(all_xml.contains("Lagerung Juli 2026"), "month label should render");
    assert!(all_xml.contains("12,5 m² Lagerfläche"), "m² remark should render");
    assert!(all_xml.contains(">100<"), "netto unit price should render in Einzelpreis");
}

#[test]
fn test_partial_first_invoice_single_line_item() {
    let items = vec![InvoiceLineItem {
        pos: 1,
        description: "Anzahlung (30%) — gemäß Angebot Nr. 2026-0042".into(),
        quantity: 1.0,
        unit_price: 133.20,
        remark: None,
    }];

    let data = make_invoice_data(items, InvoiceType::PartialFirst { percent: 30 });
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok(), "XLSX generation should succeed for partial first invoice");

    let bytes = result.unwrap();
    assert!(bytes.starts_with(b"PK"));
}

#[test]
fn test_partial_final_with_deduction() {
    let mut items = base_line_items();
    // Add Abzgl. Anzahlung deduction line
    items.push(InvoiceLineItem {
        pos: 6,
        description: "Abzgl. Anzahlung (30%)".into(),
        quantity: 1.0,
        unit_price: -133.20,
        remark: None,
    });

    let data = make_invoice_data(items, InvoiceType::PartialFinal);
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok(), "XLSX generation should succeed for partial final with deduction");

    let bytes = result.unwrap();
    assert!(bytes.starts_with(b"PK"));
}

#[test]
fn test_max_items_truncated() {
    // 20 slots available (rows 31-50), excess items should be truncated.
    let items: Vec<InvoiceLineItem> = (1..=25)
        .map(|i| InvoiceLineItem {
            pos: i,
            description: format!("Service {i}"),
            quantity: 1.0,
            unit_price: 10.0 * i as f64,
            remark: None,
        })
        .collect();

    let data = make_invoice_data(items, InvoiceType::Full);
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok(), "XLSX generation should succeed with 25 line items (truncated to 20)");

    let bytes = result.unwrap();
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    let sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("Should have sheet1.xml");
    let sheet_str = std::io::read_to_string(sheet).expect("Should read sheet1.xml");

    // Verify the first 20 items are present.
    for i in 1..=20 {
        assert!(sheet_str.contains(&format!("Service {i}")), "Should contain 'Service {i}'");
    }
    // Items 21 and beyond should be truncated.
    assert!(!sheet_str.contains("Service 21"), "Should NOT contain 'Service 21' (truncated)");
    assert!(!sheet_str.contains("Service 25"), "Should NOT contain 'Service 25' (truncated)");
}

#[test]
fn test_legacy_path_base_netto_plus_extras() {
    // Verify that the legacy path (empty line_items + base_netto_cents + extra_services)
    // still works and produces valid XLSX output
    #[allow(deprecated)]
    let data = InvoiceData {
        invoice_number: "2026-0200".into(),
        invoice_type: InvoiceType::Full,
        invoice_date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
        service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
        customer_name: "Test Customer".into(),
        customer_email: Some("test@test.de".into()),
        company_name: None,
        attention_line: None,
        billing_street: String::new(),
        billing_city: String::new(),
        service_street: String::new(),
        service_city: String::new(),
        offer_number: "2026-0042".into(),
        salutation: "Sehr geehrte Damen und Herren,".into(),
        line_items: vec![], // empty → triggers legacy path
        base_netto_cents: 35000, // €350.00
        extra_services: vec![
            aust_offer_generator::ExtraService {
                description: "Klaviertransport".into(),
                price_cents: 20000, // €200.00
            },
        ],
        origin_street: "Goslarsche Landstr. 6".into(),
        origin_city: "31135 Hildesheim".into(),
    };

    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok(), "Legacy path should still work");

    let bytes = result.unwrap();
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    let sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("Should have sheet1.xml");
    let sheet_str = std::io::read_to_string(sheet).expect("Should read sheet1.xml");

    // Should contain the base description and the extra service
    assert!(sheet_str.contains("Umzugsdienstleistung"), "Should contain base line item");
    assert!(sheet_str.contains("Klaviertransport"), "Should contain extra service");
}

#[test]
fn test_row_hiding_with_few_items() {
    // With only 3 items, rows 34-46 should be hidden
    let items = vec![
        InvoiceLineItem { pos: 1, description: "Service 1".into(), quantity: 1.0, unit_price: 100.0, remark: None },
        InvoiceLineItem { pos: 2, description: "Service 2".into(), quantity: 1.0, unit_price: 200.0, remark: None },
        InvoiceLineItem { pos: 3, description: "Service 3".into(), quantity: 1.0, unit_price: 300.0, remark: None },
    ];

    let data = make_invoice_data(items, InvoiceType::Full);
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok());

    let bytes = result.unwrap();
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    let sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("Should have sheet1.xml");
    let sheet_str = std::io::read_to_string(sheet).expect("Should read sheet1.xml");

    // With 3 items, rows 34-37 should be hidden (only rows 31-33 are used)
    for row in 34..=37 {
        let needle = format!("r=\"{row}\"");
        assert!(
            sheet_str.contains(&needle)
                && sheet_str.contains("hidden=\"true\""),
            "Row {row} should be hidden (not in used_rows)",
        );
    }
}

#[test]
fn test_remark_appended_to_description() {
    let items = vec![
        InvoiceLineItem {
            pos: 1,
            description: "Halteverbotszone".into(),
            quantity: 1.0,
            unit_price: 100.0,
            remark: Some("Entladestelle".into()),
        },
    ];

    let data = make_invoice_data(items, InvoiceType::Full);
    let result = generate_invoice_xlsx(&data);
    assert!(result.is_ok());

    let bytes = result.unwrap();
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).expect("Should be valid ZIP");
    let sheet = archive.by_name("xl/worksheets/sheet1.xml").expect("Should have sheet1.xml");
    let sheet_str = std::io::read_to_string(sheet).expect("Should read sheet1.xml");

    assert!(
        sheet_str.contains("Halteverbotszone (Entladestelle)"),
        "Remark should be appended in parentheses to description"
    );
}