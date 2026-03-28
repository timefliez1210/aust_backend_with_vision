use aust_offer_generator::{
    convert_xlsx_to_pdf, generate_offer_xlsx, DetectedItemRow, OfferData, OfferLineItem,
};
use chrono::NaiveDate;

#[tokio::main]
async fn main() {
    let data = OfferData {
        offer_number: "2026-0131".into(),
        date: NaiveDate::from_ymd_opt(2026, 3, 28).unwrap(),
        valid_until: Some(NaiveDate::from_ymd_opt(2026, 4, 28).unwrap()),
        customer_salutation: "Herrn".into(),
        customer_name: "Max Krause".into(),
        customer_street: "Musterstraße 42".into(),
        customer_city: "31135 Hildesheim".into(),
        customer_phone: "0176 12345678".into(),
        customer_email: "max.krause@example.com".into(),
        greeting: "Sehr geehrter Herr Krause,".into(),
        moving_date: "15.04.2026".into(),
        origin_street: "Alte Allee 7".into(),
        origin_city: "30161 Hannover".into(),
        origin_floor_info: "3. Stock, kein Aufzug".into(),
        dest_street: "Musterstraße 42".into(),
        dest_city: "31135 Hildesheim".into(),
        dest_floor_info: "Erdgeschoss".into(),
        volume_m3: 18.5,
        persons: 3,
        estimated_hours: 4.0,
        rate_per_person_hour: 30.0,
        line_items: vec![
            OfferLineItem {
                description: "Fahrkostenpauschale".into(),
                quantity: 1.0,
                unit_price: 0.0,
                is_labor: false,
                remark: None,
                flat_total: Some(85.0),
            },
            OfferLineItem {
                description: "Halteverbotszone".into(),
                quantity: 1.0,
                unit_price: 100.0,
                is_labor: false,
                remark: Some("Beladestelle".into()),
                flat_total: None,
            },
            OfferLineItem {
                description: "De/Montage".into(),
                quantity: 1.0,
                unit_price: 50.0,
                is_labor: false,
                remark: None,
                flat_total: None,
            },
            OfferLineItem {
                description: "3 Umzugshelfer".into(),
                quantity: 4.0,
                unit_price: 30.0,
                is_labor: true,
                remark: None,
                flat_total: None,
            },
            OfferLineItem {
                description: "Nürnbergerversicherung".into(),
                quantity: 1.0,
                unit_price: 0.0,
                is_labor: false,
                remark: None,
                flat_total: Some(0.0),
            },
        ],
        detected_items: vec![
            DetectedItemRow {
                name: "Sofa".into(),
                volume_m3: 1.2,
                dimensions: Some("200×90×80 cm".into()),
                confidence: 0.92,
                german_name: None,
                re_value: None,
                volume_source: None,
                crop_s3_key: None,
                bbox: None,
                bbox_image_index: None,
                source_image_urls: None,
            },
            DetectedItemRow {
                name: "Doppelbett".into(),
                volume_m3: 1.0,
                dimensions: Some("200×180×50 cm".into()),
                confidence: 0.88,
                german_name: None,
                re_value: None,
                volume_source: None,
                crop_s3_key: None,
                bbox: None,
                bbox_image_index: None,
                source_image_urls: None,
            },
        ],
    };

    let xlsx = generate_offer_xlsx(&data).expect("XLSX generation failed");
    let out_xlsx = "/tmp/test_offer.xlsx";
    std::fs::write(out_xlsx, &xlsx).expect("write XLSX failed");
    println!("XLSX written to {out_xlsx}");

    match convert_xlsx_to_pdf(&xlsx).await {
        Ok(pdf) => {
            let out_pdf = "/tmp/131-2026 Krause.pdf";
            std::fs::write(out_pdf, &pdf).expect("write PDF failed");
            println!("PDF written to {out_pdf}");
        }
        Err(e) => {
            eprintln!("PDF conversion failed (LibreOffice needed): {e}");
            eprintln!("XLSX is still available at {out_xlsx}");
        }
    }
}
