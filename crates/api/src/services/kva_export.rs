//! XLSX export for the KVA-Buch.
//!
//! Mirrors `register_export` and reuses its workbook plumbing (styles, parts, ZIP
//! layout) via `build_two_sheet_workbook` — only the two sheet bodies differ.
//!
//! Two sheets: the register itself, and a monthly overview. Unlike the invoice
//! register this one carries a **Lage** column instead of a payment status, because
//! a KVA is won or lost rather than paid, and that state comes from the inquiry —
//! `offers.status` is not maintained.

use chrono::NaiveDate;

use crate::services::register_export::{
    cell_ref, cell_xml, date, esc, money, style, Cell,
};
use crate::ApiError;

/// One KVA row as it appears in the export.
pub(crate) struct KvaExportRow {
    pub offer_number: String,
    pub kva_date: Option<NaiveDate>,
    pub customer: String,
    pub scheduled_date: Option<NaiveDate>,
    pub netto_cents: i64,
    pub mwst_cents: i64,
    pub brutto_cents: i64,
    /// "Gewonnen" | "Verloren" | "Offen" | "Unklar".
    pub lage: String,
    pub age_days: i64,
    /// Invoice number once the KVA became a job.
    pub invoice_number: String,
}

const HEADERS: [&str; 10] = [
    "KVA-Nummer",
    "KVA-Datum",
    "Kunde",
    "Umzugsdatum",
    "Netto",
    "MWST",
    "Brutto",
    "Lage",
    "Alter (Tage)",
    "Rechnung",
];

const WIDTHS: [f64; 10] = [14.0, 12.0, 32.0, 14.0, 12.0, 12.0, 12.0, 12.0, 13.0, 14.0];

fn sheet_xml(year: i32, rows: &[KvaExportRow]) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols>"#,
    );
    for (i, w) in WIDTHS.iter().enumerate() {
        xml.push_str(&format!(
            r#"<col min="{n}" max="{n}" width="{w}" customWidth="1"/>"#,
            n = i + 1
        ));
    }
    xml.push_str("</cols><sheetData>");

    // Title row.
    xml.push_str(&format!(
        r#"<row r="1"><c r="A1" s="{}" t="inlineStr"><is><t>{}</t></is></c></row>"#,
        style::TITLE,
        esc(&format!("KVA-Buch {year}"))
    ));

    // Header row.
    xml.push_str(r#"<row r="2">"#);
    for (i, h) in HEADERS.iter().enumerate() {
        xml.push_str(&cell_xml(i, 2, &Cell::Text((*h).to_string()), style::HEADER));
    }
    xml.push_str("</row>");

    // Data rows start at 3.
    for (n, r) in rows.iter().enumerate() {
        let row = n + 3;
        let cells = [
            (Cell::Text(r.offer_number.clone()), style::PLAIN),
            (date(r.kva_date), style::DATE),
            (Cell::Text(r.customer.clone()), style::PLAIN),
            (date(r.scheduled_date), style::DATE),
            (money(Some(r.netto_cents)), style::MONEY),
            (money(Some(r.mwst_cents)), style::MONEY),
            (money(Some(r.brutto_cents)), style::MONEY),
            (Cell::Text(r.lage.clone()), style::PLAIN),
            (Cell::Number(r.age_days as f64), style::PLAIN),
            (Cell::Text(r.invoice_number.clone()), style::PLAIN),
        ];
        xml.push_str(&format!(r#"<row r="{row}">"#));
        for (i, (cell, st)) in cells.iter().enumerate() {
            xml.push_str(&cell_xml(i, row, cell, *st));
        }
        xml.push_str("</row>");
    }

    // Totals.
    let total_row = rows.len() + 3;
    let netto: i64 = rows.iter().map(|r| r.netto_cents).sum();
    let mwst: i64 = rows.iter().map(|r| r.mwst_cents).sum();
    let brutto: i64 = rows.iter().map(|r| r.brutto_cents).sum();
    xml.push_str(&format!(r#"<row r="{total_row}">"#));
    xml.push_str(&cell_xml(
        0,
        total_row,
        &Cell::Text(format!("Summe {year}")),
        style::TOTAL_TEXT,
    ));
    for i in 1..=3 {
        xml.push_str(&cell_xml(i, total_row, &Cell::Empty, style::TOTAL_TEXT));
    }
    for (i, v) in [netto, mwst, brutto].iter().enumerate() {
        xml.push_str(&cell_xml(4 + i, total_row, &money(Some(*v)), style::TOTAL_MONEY));
    }
    for i in 7..HEADERS.len() {
        xml.push_str(&cell_xml(i, total_row, &Cell::Empty, style::TOTAL_TEXT));
    }
    xml.push_str("</row></sheetData>");

    // Freeze the header so the register scrolls under it.
    xml.push_str(&format!(
        r#"<sheetView workbookViewId="0"><pane ySplit="2" topLeftCell="{}" activePane="bottomLeft" state="frozen"/></sheetView>"#,
        cell_ref(0, 3)
    ));
    xml.push_str("</worksheet>");
    xml
}

const SUMMARY_HEADERS: [&str; 7] = [
    "Monat",
    "KVAs",
    "Volumen netto",
    "Gewonnen netto",
    "Gewonnen",
    "Verloren",
    "Offen",
];

const MONTH_NAMES: [&str; 12] = [
    "Januar", "Februar", "März", "April", "Mai", "Juni",
    "Juli", "August", "September", "Oktober", "November", "Dezember",
];

/// Per-month rollup: volume quoted, volume won, and the win/loss/open counts.
fn summary_sheet_xml(year: i32, rows: &[KvaExportRow]) -> String {
    // (count, volume, won_volume, won, lost, open)
    let mut buckets = [(0i64, 0i64, 0i64, 0i64, 0i64, 0i64); 13];

    for r in rows {
        // Rows whose KVA date falls outside the exported year land in the spare
        // 13th bucket rather than being silently dropped.
        let idx = match r.kva_date {
            Some(d) if d.format("%Y").to_string() == year.to_string() => {
                d.format("%m").to_string().parse::<usize>().unwrap_or(1) - 1
            }
            _ => 12,
        };
        let b = &mut buckets[idx];
        b.0 += 1;
        b.1 += r.netto_cents;
        match r.lage.as_str() {
            "Gewonnen" => {
                b.2 += r.netto_cents;
                b.3 += 1;
            }
            "Verloren" => b.4 += 1,
            "Offen" => b.5 += 1,
            _ => {}
        }
    }

    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols>"#,
    );
    for (i, w) in [16.0, 10.0, 16.0, 16.0, 12.0, 12.0, 10.0].iter().enumerate() {
        xml.push_str(&format!(
            r#"<col min="{n}" max="{n}" width="{w}" customWidth="1"/>"#,
            n = i + 1
        ));
    }
    xml.push_str("</cols><sheetData>");

    xml.push_str(&format!(
        r#"<row r="1"><c r="A1" s="{}" t="inlineStr"><is><t>{}</t></is></c></row>"#,
        style::TITLE,
        esc(&format!("Monatsübersicht {year}"))
    ));

    xml.push_str(r#"<row r="2">"#);
    for (i, h) in SUMMARY_HEADERS.iter().enumerate() {
        xml.push_str(&cell_xml(i, 2, &Cell::Text((*h).to_string()), style::HEADER));
    }
    xml.push_str("</row>");

    let write_row = |xml: &mut String, n: usize, label: &str, b: &(i64, i64, i64, i64, i64, i64)| {
        let row = n;
        xml.push_str(&format!(r#"<row r="{row}">"#));
        xml.push_str(&cell_xml(0, row, &Cell::Text(label.to_string()), style::PLAIN));
        xml.push_str(&cell_xml(1, row, &Cell::Number(b.0 as f64), style::PLAIN));
        xml.push_str(&cell_xml(2, row, &money(Some(b.1)), style::MONEY));
        xml.push_str(&cell_xml(3, row, &money(Some(b.2)), style::MONEY));
        xml.push_str(&cell_xml(4, row, &Cell::Number(b.3 as f64), style::PLAIN));
        xml.push_str(&cell_xml(5, row, &Cell::Number(b.4 as f64), style::PLAIN));
        xml.push_str(&cell_xml(6, row, &Cell::Number(b.5 as f64), style::PLAIN));
        xml.push_str("</row>");
    };

    for (i, name) in MONTH_NAMES.iter().enumerate() {
        write_row(&mut xml, i + 3, name, &buckets[i]);
    }
    if buckets[12].0 > 0 {
        write_row(&mut xml, 15, "ohne Monat", &buckets[12]);
    }

    // Year total.
    let total_row = if buckets[12].0 > 0 { 16 } else { 15 };
    let t = buckets.iter().fold((0i64, 0i64, 0i64, 0i64, 0i64, 0i64), |a, b| {
        (a.0 + b.0, a.1 + b.1, a.2 + b.2, a.3 + b.3, a.4 + b.4, a.5 + b.5)
    });
    xml.push_str(&format!(r#"<row r="{total_row}">"#));
    xml.push_str(&cell_xml(0, total_row, &Cell::Text("Jahr".into()), style::TOTAL_TEXT));
    xml.push_str(&cell_xml(1, total_row, &Cell::Number(t.0 as f64), style::TOTAL_TEXT));
    xml.push_str(&cell_xml(2, total_row, &money(Some(t.1)), style::TOTAL_MONEY));
    xml.push_str(&cell_xml(3, total_row, &money(Some(t.2)), style::TOTAL_MONEY));
    xml.push_str(&cell_xml(4, total_row, &Cell::Number(t.3 as f64), style::TOTAL_TEXT));
    xml.push_str(&cell_xml(5, total_row, &Cell::Number(t.4 as f64), style::TOTAL_TEXT));
    xml.push_str(&cell_xml(6, total_row, &Cell::Number(t.5 as f64), style::TOTAL_TEXT));
    xml.push_str("</row>");

    xml.push_str("</sheetData></worksheet>");
    xml
}

pub(crate) fn build_xlsx(year: i32, rows: &[KvaExportRow]) -> Result<Vec<u8>, ApiError> {
    crate::services::register_export::build_two_sheet_workbook(
        [&format!("KVA {year}"), "Monatsübersicht"],
        sheet_xml(year, rows),
        summary_sheet_xml(year, rows),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
    }

    fn row(lage: &str, month: u32, netto: i64) -> KvaExportRow {
        KvaExportRow {
            offer_number: "2026-0001".into(),
            kva_date: Some(d(2026, month, 15)),
            customer: "Test Kunde".into(),
            scheduled_date: Some(d(2026, month, 28)),
            netto_cents: netto,
            mwst_cents: netto * 19 / 100,
            brutto_cents: netto + netto * 19 / 100,
            lage: lage.into(),
            age_days: 12,
            invoice_number: String::new(),
        }
    }

    #[test]
    fn builds_a_non_empty_zip_with_both_sheets() {
        let bytes = build_xlsx(2026, &[row("Gewonnen", 3, 100_000)]).expect("xlsx");
        assert!(bytes.len() > 500, "suspiciously small workbook");
        // ZIP local file header.
        assert_eq!(&bytes[0..2], b"PK");
    }

    #[test]
    fn sheet_carries_every_header_and_the_year_title() {
        let xml = sheet_xml(2026, &[row("Offen", 5, 50_000)]);
        assert!(xml.contains("KVA-Buch 2026"), "{xml}");
        for h in HEADERS {
            assert!(xml.contains(h), "missing header {h}");
        }
    }

    #[test]
    fn totals_row_sums_the_money_columns() {
        let xml = sheet_xml(2026, &[row("Gewonnen", 1, 100_000), row("Verloren", 2, 300_000)]);
        // 400000 cents = 4000.00
        assert!(xml.contains("<v>4000</v>"), "netto total missing: {xml}");
    }

    #[test]
    fn summary_buckets_by_month_and_splits_won_from_lost() {
        let xml = summary_sheet_xml(
            2026,
            &[
                row("Gewonnen", 3, 100_000),
                row("Verloren", 3, 300_000),
                row("Offen", 3, 50_000),
            ],
        );
        assert!(xml.contains("Monatsübersicht 2026"));
        // März row: 3 KVAs, volume 4500.00, won 1000.00
        assert!(xml.contains("<v>4500</v>"), "march volume missing: {xml}");
        assert!(xml.contains("<v>1000</v>"), "march won missing: {xml}");
    }

    /// A KVA dated outside the exported year must be visible, not silently dropped.
    #[test]
    fn out_of_year_rows_land_in_their_own_bucket() {
        let mut r = row("Gewonnen", 3, 100_000);
        r.kva_date = Some(d(2025, 12, 30));
        let xml = summary_sheet_xml(2026, &[r]);
        assert!(xml.contains("ohne Monat"), "{xml}");
    }

    /// XML metacharacters in a customer name must not break the part.
    #[test]
    fn escapes_customer_names() {
        let mut r = row("Offen", 4, 10_000);
        r.customer = "Meier & Söhne <GmbH>".into();
        let xml = sheet_xml(2026, &[r]);
        assert!(xml.contains("Meier &amp; Söhne &lt;GmbH&gt;"), "{xml}");
        assert!(!xml.contains("<GmbH>"));
    }

    #[test]
    fn handles_an_empty_year() {
        let bytes = build_xlsx(2026, &[]).expect("xlsx");
        assert!(bytes.len() > 500);
    }
}
