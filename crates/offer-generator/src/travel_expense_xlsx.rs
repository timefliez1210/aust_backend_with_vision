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
    modified_sheet1 = set_cell_value(&modified_sheet1, "D18", &CellValue::Number(date_to_excel_serial(data.start_date)));
    modified_sheet1 = set_cell_value(&modified_sheet1, "G18", &CellValue::Number(time_to_excel_fraction(data.start_time)));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D19", &CellValue::Number(date_to_excel_serial(data.end_date)));
    modified_sheet1 = set_cell_value(&modified_sheet1, "G19", &CellValue::Number(time_to_excel_fraction(data.end_time)));

    // Destination & Reason
    modified_sheet1 = set_cell_value(&modified_sheet1, "D20", &CellValue::Text(data.destination.clone()));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D21", &CellValue::Text(data.reason.clone()));

    // Transport
    let transport_text = match &data.transport_mode {
        Some(mode) => format!("[x] {mode}"),
        None => String::new(),
    };
    modified_sheet1 = set_cell_value(&modified_sheet1, "D22", &CellValue::Text(transport_text));

    // Costs
    modified_sheet1 = set_cell_value(&modified_sheet1, "D26", &CellValue::Number(data.travel_costs_eur));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D27", &CellValue::Number(data.small_days as f64));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D29", &CellValue::Number(data.large_days as f64));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D30", &CellValue::Number(data.breakfast_deduction_eur));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D31", &CellValue::Number(data.meal_deduction_eur));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D32", &CellValue::Number(data.accommodation_eur));
    modified_sheet1 = set_cell_value(&modified_sheet1, "D33", &CellValue::Number(data.misc_costs_eur));

    // We also strip extra worksheets and fix the workbook to only reference sheet1.
    let workbook_xml = read_zip_entry(&mut template_zip, "xl/workbook.xml")?;
    let workbook_str = String::from_utf8(workbook_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml is not valid UTF-8: {e}")))?;
    let modified_workbook = strip_extra_sheets(&workbook_str);

    // Fix content types and rels to only reference one sheet
    let content_types_xml = read_zip_entry(&mut template_zip, "[Content_Types].xml")?;
    let content_types_str = String::from_utf8(content_types_xml)
        .map_err(|e| OfferError::Template(format!("Content_Types.xml not valid UTF-8: {e}")))?;
    let modified_content_types = strip_extra_sheet_content_types(&content_types_str);

    let rels_xml = read_zip_entry(&mut template_zip, "xl/_rels/workbook.xml.rels")?;
    let rels_str = String::from_utf8(rels_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml.rels not valid UTF-8: {e}")))?;
    let modified_rels = strip_extra_sheet_rels(&rels_str);

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
    let mut file = zip.by_name(name)
        .map_err(|e| OfferError::Template(format!("Missing {name} in template: {e}")))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| OfferError::Template(format!("Failed to read {name}: {e}")))?;
    Ok(buf)
}

fn strip_extra_sheets(workbook_xml: &str) -> String {
    // Keep only the first <sheet> element inside <sheets>...</sheets>
    let sheets_open = "<sheets>";
    let sheets_close = "</sheets>";
    let Some(start) = workbook_xml.find(sheets_open) else {
        return workbook_xml.to_string();
    };
    let Some(end) = workbook_xml.find(sheets_close) else {
        return workbook_xml.to_string();
    };
    let prefix = &workbook_xml[..start + sheets_open.len()];
    let suffix = &workbook_xml[end..];

    let inner = &workbook_xml[start + sheets_open.len()..end];
    let first_sheet_start = inner.find("<sheet ");
    let first_sheet_end = inner[first_sheet_start.map(|s| s + 1).unwrap_or(0)..].find("/>");

    let sheet_tag = match (first_sheet_start, first_sheet_end) {
        (Some(s), Some(e)) => &inner[s..s + 1 + e + 2],
        _ => "",
    };

    format!("{}\n{}\n{}", prefix, sheet_tag, suffix)
}

fn strip_extra_sheet_content_types(ct_xml: &str) -> String {
    // Remove all Override entries for sheet2.xml, sheet3.xml, etc.
    let mut result = ct_xml.to_string();
    for i in 2..=20 {
        let pattern = format!(r#"PartName="/xl/worksheets/sheet{}.xml""#, i);
        result = result.lines()
            .filter(|line| !line.contains(&pattern))
            .collect::<Vec<_>>()
            .join("\n");
    }
    result
}

fn strip_extra_sheet_rels(rels_xml: &str) -> String {
    // Remove all Relationship entries targeting sheet2.xml, sheet3.xml, etc.
    let mut result = rels_xml.to_string();
    for i in 2..=20 {
        let pattern = format!(r#"Target="worksheets/sheet{}.xml""#, i);
        result = result.lines()
            .filter(|line| !line.contains(&pattern))
            .collect::<Vec<_>>()
            .join("\n");
    }
    result
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
            let mut entry = template_zip.by_index(i)
                .map_err(|e| OfferError::Template(format!("Zip read error: {e}")))?;
            let name = entry.name().to_string();

            let mut buf = Vec::new();
            std::io::copy(&mut entry, &mut buf)
                .map_err(|e| OfferError::Template(format!("Zip copy error: {e}")))?;

            // Skip extra worksheet XMLs and their rels
            if name.starts_with("xl/worksheets/sheet") && name != "xl/worksheets/sheet1.xml" {
                continue;
            }
            if name.starts_with("xl/worksheets/_rels/sheet") && name != "xl/worksheets/_rels/sheet1.xml.rels" {
                continue;
            }

            let data: Vec<u8> = match name.as_str() {
                "xl/worksheets/sheet1.xml" => sheet1_xml.as_bytes().to_vec(),
                "xl/workbook.xml" => workbook_xml.as_bytes().to_vec(),
                "[Content_Types].xml" => content_types_xml.as_bytes().to_vec(),
                "xl/_rels/workbook.xml.rels" => rels_xml.as_bytes().to_vec(),
                _ => buf,
            };

            out_zip.start_file(&name, options)
                .map_err(|e| OfferError::Template(format!("Zip start_file error: {e}")))?;
            out_zip.write_all(&data)
                .map_err(|e| OfferError::Template(format!("Zip write error: {e}")))?;
        }

        out_zip.finish()
            .map_err(|e| OfferError::Template(format!("Zip finish error: {e}")))?;
    }
    Ok(output)
}
