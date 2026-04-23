//! Travel expense (Reisekostenabrechnung) XLSX document generation.
//!
//! Fills the Reisekostenabrechnung template with trip data for a single employee.

use std::io::{Cursor, Read, Write};
use zip::ZipArchive;
use chrono::Timelike;

use crate::error::OfferError;
use crate::xlsx::{set_cell_value, CellValue};

const TEMPLATE_BYTES: &[u8] = include_bytes!("../../../templates/reisekosten_template.xlsx");

/// Data needed to fill a travel expense form.
pub struct TravelExpenseData {
    pub employee_first_name: String,
    pub employee_last_name: String,
    pub start_date: chrono::NaiveDate,
    pub start_time: chrono::NaiveTime,
    pub end_date: chrono::NaiveDate,
    pub end_time: chrono::NaiveTime,
    pub destination: String,
    pub reason: String,
    pub transport_mode: Option<String>,
    pub travel_costs_eur: f64,
    pub small_days: i32,
    pub large_days: i32,
    pub breakfast_deduction_eur: f64,
    pub meal_deduction_eur: f64,
    pub accommodation_eur: f64,
    pub misc_costs_eur: f64,
}

/// Convert a `NaiveDate` to an Excel serial number (Windows date system).
///
/// Excel's epoch is 1899-12-30, with the 1900 leap-year bug (non-existent
/// 1900-02-29 is counted as day 60, so all serials ≥ 61 are off by +1).
fn date_to_excel_serial(date: chrono::NaiveDate) -> f64 {
    let epoch = chrono::NaiveDate::from_ymd_opt(1899, 12, 30).unwrap();
    let days = (date - epoch).num_days();
    if days > 60 {
        (days + 1) as f64
    } else {
        days as f64
    }
}

/// Convert a `NaiveTime` to an Excel time fraction (0.0 = midnight, 0.5 = noon).
fn time_to_excel_fraction(time: chrono::NaiveTime) -> f64 {
    let seconds = time.num_seconds_from_midnight() as f64;
    seconds / 86400.0
}

/// Generate a filled travel-expense XLSX for one employee.
///
/// Uses the first worksheet of the template (sheet1.xml). The other worksheets
/// are stripped so the output contains a single clean form.
pub fn generate_travel_expense_xlsx(data: &TravelExpenseData) -> Result<Vec<u8>, OfferError> {
    let mut template_zip = ZipArchive::new(Cursor::new(TEMPLATE_BYTES))
        .map_err(|e| OfferError::Template(format!("Failed to read travel-expense template ZIP: {e}")))?;

    // Read the first sheet
    let sheet1_xml = read_zip_entry(&mut template_zip, "xl/worksheets/sheet1.xml")?;
    let sheet1_str = String::from_utf8(sheet1_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml is not valid UTF-8: {e}")))?;

    // Apply cell modifications
    let mut modified_sheet1 = sheet1_str;

    // Reisedaten
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D18",
        &CellValue::Number(date_to_excel_serial(data.start_date)),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "G18",
        &CellValue::Number(time_to_excel_fraction(data.start_time)),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D19",
        &CellValue::Number(date_to_excel_serial(data.end_date)),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "G19",
        &CellValue::Number(time_to_excel_fraction(data.end_time)),
    );

    // Employee name (overwrites pre-filled template data)
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D13",
        &CellValue::Text(data.employee_first_name.clone()),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "G13",
        &CellValue::Text(data.employee_last_name.clone()),
    );

    // Destination & Reason
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D20",
        &CellValue::Text(data.destination.clone()),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D21",
        &CellValue::Text(data.reason.clone()),
    );

    // Transport
    let transport_text = match &data.transport_mode {
        Some(mode) => format!("[x] {mode}"),
        None => String::new(),
    };
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D22",
        &CellValue::Text(transport_text),
    );

    // Costs
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D26",
        &CellValue::Number(data.travel_costs_eur),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D27",
        &CellValue::Number(data.small_days as f64),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D29",
        &CellValue::Number(data.large_days as f64),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D30",
        &CellValue::Number(data.breakfast_deduction_eur),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D31",
        &CellValue::Number(data.meal_deduction_eur),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D32",
        &CellValue::Number(data.accommodation_eur),
    );
    modified_sheet1 = set_cell_value(
        &modified_sheet1,
        "D33",
        &CellValue::Number(data.misc_costs_eur),
    );

    // Fix workbook.xml — keep only first sheet, rename to "Reisekosten", set activeTab to 0
    let workbook_xml = read_zip_entry(&mut template_zip, "xl/workbook.xml")?;
    let workbook_str = String::from_utf8(workbook_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml is not valid UTF-8: {e}")))?;
    let modified_workbook = fix_workbook_single_sheet(&workbook_str);

    // Fix content types — remove extra sheet overrides via proper string replacement
    let content_types_xml = read_zip_entry(&mut template_zip, "[Content_Types].xml")?;
    let content_types_str = String::from_utf8(content_types_xml)
        .map_err(|e| OfferError::Template(format!("Content_Types.xml not valid UTF-8: {e}")))?;
    let modified_content_types = remove_extra_sheet_overrides(&content_types_str);

    // Fix sheet rels — remove extra worksheet rels
    let rels_xml = read_zip_entry(&mut template_zip, "xl/_rels/workbook.xml.rels")?;
    let rels_str = String::from_utf8(rels_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml.rels not valid UTF-8: {e}")))?;
    let modified_rels = remove_extra_sheet_rels(&rels_str);

    // Assemble output ZIP
    assemble_xlsx_single_sheet(
        &mut template_zip,
        &modified_sheet1,
        &modified_workbook,
        &modified_content_types,
        &modified_rels,
    )
}

fn read_zip_entry(zip: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, OfferError> {
    let mut file = zip
        .by_name(name)
        .map_err(|e| OfferError::Template(format!("Missing {name} in template: {e}")))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| OfferError::Template(format!("Failed to read {name}: {e}")))?;
    Ok(buf)
}

/// Rewrite workbook.xml to contain exactly one sheet named "Reisekosten"
/// with sheetId="1" and activeTab="0".
fn fix_workbook_single_sheet(xml: &str) -> String {
    // Strategy: find the <sheets> block, replace everything inside with a single
    // sheet element, and fix activeTab="..." to activeTab="0".
    let sheets_open = "<sheets>";
    let sheets_close = "</sheets>";
    let Some(start) = xml.find(sheets_open) else {
        return xml.to_string();
    };
    let Some(end) = xml.find(sheets_close) else {
        return xml.to_string();
    };
    let before = &xml[..start + sheets_open.len()];
    let after = &xml[end..];

    // First, try to extract the rId of the FIRST <sheet> element
    let inner = &xml[start + sheets_open.len()..end];
    let rid = inner
        .split("r:id=")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .unwrap_or("rId1");

    let replacement = format!(
        r#"<sheet name="Reisekosten" sheetId="1" r:id="{}"/>"#,
        rid
    );

    let mut result = format!("{}\n{}\n{}", before, replacement, after);

    // Fix activeTab — any activeTab="N" where N > 0 should become "0"
    // (the first and now only sheet).
    result = result.replace("activeTab=\"5\"", "activeTab=\"0\"");
    result = result.replace("activeTab=\"4\"", "activeTab=\"0\"");
    result = result.replace("activeTab=\"3\"", "activeTab=\"0\"");
    result = result.replace("activeTab=\"2\"", "activeTab=\"0\"");
    result = result.replace("activeTab=\"1\"", "activeTab=\"0\"");

    result
}

/// Remove all `<Override PartName="/xl/worksheets/sheetN.xml" …/>` entries
/// for N ≥ 2 from Content_Types.xml using a regex that works on single-line XML.
fn remove_extra_sheet_overrides(xml: &str) -> String {
    // Match: <Override … PartName="/xl/worksheets/sheetN.xml" …/>
    // Use regex to find these exact tags and remove them.
    let re = regex::Regex::new(
        r#"<Override\s+[^>]*PartName="/xl/worksheets/sheet[2-9][^"]*"[^>]*/>"#,
    )
    .unwrap_or_else(|_| regex::Regex::new("a^").unwrap());
    re.replace_all(xml, "").to_string()
}

/// Remove all `<Relationship … Target="worksheets/sheetN.xml" …/>` entries
/// for N ≥ 2 from workbook.xml.rels using a regex that works on single-line XML.
fn remove_extra_sheet_rels(xml: &str) -> String {
    let re = regex::Regex::new(
        r#"<Relationship\s+[^>]*Target="worksheets/sheet[2-9][^"]*"[^>]*/>"#,
    )
    .unwrap_or_else(|_| regex::Regex::new("a^").unwrap());
    re.replace_all(xml, "").to_string()
}

fn assemble_xlsx_single_sheet(
    template_zip: &mut ZipArchive<Cursor<&[u8]>>,
    sheet1_xml: &str,
    workbook_xml: &str,
    content_types_xml: &str,
    rels_xml: &str,
) -> Result<Vec<u8>, OfferError> {
    let mut output = Vec::new();
    {
        let mut out_zip = zip::ZipWriter::new(Cursor::new(&mut output));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for i in 0..template_zip.len() {
            let mut entry = template_zip
                .by_index(i)
                .map_err(|e| OfferError::Template(format!("Zip read error: {e}")))?;
            let name = entry.name().to_string();

            let mut buf = Vec::new();
            std::io::copy(&mut entry, &mut buf)
                .map_err(|e| OfferError::Template(format!("Zip copy error: {e}")))?;

            // Skip extra worksheet XMLs and their rels
            if name.starts_with("xl/worksheets/sheet") && name != "xl/worksheets/sheet1.xml" {
                continue;
            }
            if name.starts_with("xl/worksheets/_rels/sheet")
                && name != "xl/worksheets/_rels/sheet1.xml.rels"
            {
                continue;
            }

            let data: Vec<u8> = match name.as_str() {
                "xl/worksheets/sheet1.xml" => sheet1_xml.as_bytes().to_vec(),
                "xl/workbook.xml" => workbook_xml.as_bytes().to_vec(),
                "[Content_Types].xml" => content_types_xml.as_bytes().to_vec(),
                "xl/_rels/workbook.xml.rels" => rels_xml.as_bytes().to_vec(),
                _ => buf,
            };

            out_zip
                .start_file(&name, options)
                .map_err(|e| OfferError::Template(format!("Zip start_file error: {e}")))?;
            out_zip
                .write_all(&data)
                .map_err(|e| OfferError::Template(format!("Zip write error: {e}")))?;
        }

        out_zip
            .finish()
            .map_err(|e| OfferError::Template(format!("Zip finish error: {e}")))?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    #[test]
    fn test_generate_travel_expense_smoke() {
        let data = TravelExpenseData {
            employee_first_name: "Max".into(),
            employee_last_name: "Mustermann".into(),
            start_date: NaiveDate::from_ymd_opt(2026, 3, 23).unwrap(),
            start_time: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2026, 3, 26).unwrap(),
            end_time: NaiveTime::from_hms_opt(16, 30, 0).unwrap(),
            destination: "Rüsselsheim".into(),
            reason: "Luttert Montage".into(),
            transport_mode: Some("PKW".into()),
            travel_costs_eur: 0.0,
            small_days: 2,
            large_days: 2,
            breakfast_deduction_eur: 0.0,
            meal_deduction_eur: 0.0,
            accommodation_eur: 0.0,
            misc_costs_eur: 0.0,
        };
        let bytes = generate_travel_expense_xlsx(&data).expect("should generate");
        assert!(!bytes.is_empty());

        // Verify it is a valid ZIP containing expected entries
        let mut zip = zip::ZipArchive::new(Cursor::new(&bytes[..])).expect("valid zip");
        let mut names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
        names.sort();
        assert!(names.contains(&"xl/worksheets/sheet1.xml".to_string()));
        assert!(names.contains(&"xl/workbook.xml".to_string()));
        assert!(names.contains(&"[Content_Types].xml".to_string()));

        // Read all entries into memory so we don't hold multiple mutable borrows
        let ct = {
            let mut f = zip.by_name("[Content_Types].xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(!ct.contains("sheet2.xml"));

        let wb = {
            let mut f = zip.by_name("xl/workbook.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        let sheet_count = wb.matches("<sheet ").count();
        assert_eq!(sheet_count, 1, "workbook should contain exactly one sheet");
        assert!(wb.contains("name=\"Reisekosten\""));
        assert!(wb.contains("activeTab=\"0\""));

        let rels = {
            let mut f = zip.by_name("xl/_rels/workbook.xml.rels").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(!rels.contains("sheet2.xml"));

        let sheet1 = {
            let mut f = zip.by_name("xl/worksheets/sheet1.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(
            sheet1.contains("Max"),
            "sheet1 should contain the employee first name"
        );
        assert!(
            sheet1.contains("Mustermann"),
            "sheet1 should contain the employee last name"
        );
    }
}
