//! The Auftragsort line (A27) on the Rechnung.
//!
//! A move runs Beladestelle → [Zwischenstopp] → Entladestelle. The invoice names
//! the two ends of that route and nothing else:
//!
//! - the Zwischenstopp is priced, not a place the job was performed *for*, and
//!   a third address wrapped the merged A27:E28 block into the line items;
//! - the Rechnungsadresse is a separate thing entirely (it heads the letter at
//!   A10/A11) and must never be printed as a place of service.

use aust_offer_generator::{generate_invoice_xlsx, InvoiceData, InvoiceLineItem, InvoiceType};
use chrono::NaiveDate;

fn data() -> InvoiceData {
    #[allow(deprecated)]
    InvoiceData {
        invoice_number: "2026-0131".into(),
        invoice_type: InvoiceType::Full,
        invoice_date: NaiveDate::from_ymd_opt(2026, 4, 14).unwrap(),
        service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
        customer_name: "Herrn Horst Lindenthal".into(),
        customer_email: None,
        company_name: None,
        attention_line: None,
        // Rechnungsadresse — deliberately a third, unrelated address.
        billing_street: "Postfach 12".into(),
        billing_city: "30159 Hannover".into(),
        service_street: "Steinbergstr. 3".into(),
        service_city: "31139 Hildesheim".into(),
        destination_street: "Kirchweg 6".into(),
        destination_city: "31162 Bad Salzdetfurth".into(),
        offer_number: "2026-0042".into(),
        salutation: "Sehr geehrter Herr Lindenthal,".into(),
        line_items: vec![InvoiceLineItem {
            pos: 1,
            description: "Umzugsarbeiten".into(),
            quantity: 1.0,
            unit_price: 500.0,
            remark: None,
        }],
        base_netto_cents: 0,
        extra_services: vec![],
        origin_street: String::new(),
        origin_city: String::new(),
    }
}

/// Every XML part of the generated workbook, concatenated — cell text may live
/// inline or in `sharedStrings.xml` depending on which path wrote it.
fn all_xml(bytes: Vec<u8>) -> String {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("output should be a valid XLSX");
    let mut out = String::new();
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).unwrap();
        if f.name().ends_with(".xml") {
            out.push_str(&std::io::read_to_string(&mut f).unwrap_or_default());
        }
    }
    out
}

#[test]
fn prints_belade_and_entladestelle() {
    let xml = all_xml(generate_invoice_xlsx(&data()).expect("generation should succeed"));

    assert!(
        xml.contains(
            "Auftragsort: Steinbergstr. 3, 31139 Hildesheim – Kirchweg 6, 31162 Bad Salzdetfurth"
        ),
        "both ends of the route belong on the Auftragsort line"
    );
}

#[test]
fn never_prints_the_billing_address_as_a_place_of_service() {
    let xml = all_xml(generate_invoice_xlsx(&data()).expect("generation should succeed"));

    let auftragsort = xml
        .split("Auftragsort: ")
        .nth(1)
        .expect("the Auftragsort line should be written")
        .split('<')
        .next()
        .unwrap()
        .to_string();

    assert!(
        !auftragsort.contains("Postfach 12") && !auftragsort.contains("30159 Hannover"),
        "the Rechnungsadresse is not an Auftragsort, got: {auftragsort}"
    );
}

#[test]
fn a_single_address_stays_a_single_address() {
    // Entrümpelung / Lagerung: no Entladestelle, so no separator and no dangling comma.
    let mut d = data();
    d.destination_street = String::new();
    d.destination_city = String::new();
    let xml = all_xml(generate_invoice_xlsx(&d).expect("generation should succeed"));

    assert!(xml.contains("Auftragsort: Steinbergstr. 3, 31139 Hildesheim"));
    assert!(
        !xml.contains("Auftragsort: Steinbergstr. 3, 31139 Hildesheim –"),
        "no trailing separator when there is nothing to separate"
    );
}

#[test]
fn a_move_within_one_address_is_not_printed_twice() {
    let mut d = data();
    d.destination_street = d.service_street.clone();
    d.destination_city = d.service_city.clone();
    let xml = all_xml(generate_invoice_xlsx(&d).expect("generation should succeed"));

    assert!(xml.contains("Auftragsort: Steinbergstr. 3, 31139 Hildesheim"));
    assert!(!xml.contains("Hildesheim – Steinbergstr. 3"));
}

#[test]
fn without_an_origin_the_invoice_falls_back_to_the_billing_address() {
    // Pre-existing behaviour: an inquiry with no origin address row still has to
    // name somewhere, and the letter head address is the only thing left.
    let mut d = data();
    d.service_street = String::new();
    d.service_city = String::new();
    d.destination_street = String::new();
    d.destination_city = String::new();
    let xml = all_xml(generate_invoice_xlsx(&d).expect("generation should succeed"));

    assert!(xml.contains("Auftragsort: Postfach 12, 30159 Hannover"));
}
