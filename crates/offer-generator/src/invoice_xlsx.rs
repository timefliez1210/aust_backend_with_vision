//! Direct XLSX template manipulation for invoice (Rechnung) generation.
//!
//! Same ZIP/XML approach as `xlsx.rs` but simplified for the invoice template:
//! - No row hiding (unused rows show 0, which is fine for the table layout)
//! - No second sheet
//! - No hyperlink stripping (company email/website links are static and stay)
//! - 5 columns: Pos. | Bezeichnung | Menge | Einzelpreis | Gesamtpreis
//! - 7 line item slots (rows 31–37), formula-driven totals (rows 38–41)

use crate::OfferError;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// The invoice template XLSX — embedded at compile time.
const TEMPLATE_BYTES: &[u8] = include_bytes!("../../../templates/Rechnung_Vorlage.xlsx");

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// All data needed to fill the Rechnung XLSX template for one invoice.
///
/// **Caller**: `crates/api/src/routes/invoices.rs`
/// **Why**: Single transfer object between the HTTP route and the XLSX generator.
/// Amounts are always in cents (netto); the template handles 19% MwSt automatically.
#[derive(Debug, Clone)]
pub struct InvoiceData {
    /// Invoice number printed in the heading, e.g. `"12026"`.
    pub invoice_number: String,
    /// Invoice type — determines the base line item description.
    pub invoice_type: InvoiceType,
    /// Invoice creation date written to E19 (replaces `=TODAY()` formula).
    pub invoice_date: NaiveDate,
    /// Moving date written to C19 (Leistungsdatum). `None` → blank.
    pub service_date: Option<NaiveDate>,
    /// Customer full name for the address block (A8).
    pub customer_name: String,
    /// Customer email address (A9).
    pub customer_email: String,
    /// Origin street + house number (A10, A27).
    pub origin_street: String,
    /// Origin postal code + city, e.g. `"31135 Hildesheim"` (A11).
    pub origin_city: String,
    /// Offer number used in line item description, e.g. `"2026-0042"`.
    pub offer_number: String,
    /// Netto amount for the base line item in cents.
    /// For `Full`: offer netto.
    /// For `PartialFirst`: `round(offer_brutto * percent / 100 / 1.19 * 100)`.
    /// For `PartialFinal`: `round((offer_brutto * (100-percent) / 100) / 1.19 * 100)`.
    pub base_netto_cents: i64,
    /// Extra services appended as additional line items (rows 32–37, max 6).
    /// Only non-empty for `Full` and `PartialFinal`.
    pub extra_services: Vec<ExtraService>,
    /// Formal salutation line, e.g. `"Sehr geehrter Herr Müller,"`.
    pub salutation: String,
}

/// A single extra service sold on-site, appended after the base line item.
///
/// **Caller**: `crates/api/src/routes/invoices.rs` — loaded from `invoices.extra_services`
/// **Why**: Additional items that were not in the original offer (e.g. piano transport,
/// extra packaging) are billed separately on the final or full invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraService {
    /// Short description shown in column B, e.g. `"Klaviertransport"`.
    pub description: String,
    /// Netto price in cents. Written to Einzelpreis (D column);
    /// the template formula calculates Gesamtpreis = Menge × Einzelpreis.
    pub price_cents: i64,
}

/// Determines the base line item description and amount split logic.
#[derive(Debug, Clone, PartialEq)]
pub enum InvoiceType {
    /// Single invoice for the full job amount.
    Full,
    /// First of two partial invoices — Anzahlung at `percent`%.
    PartialFirst { percent: u8 },
    /// Second of two partial invoices — Restbetrag for the remaining amount.
    PartialFinal,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Generate a complete invoice XLSX from the embedded Rechnung template.
///
/// **Caller**: `crates/api/src/routes/invoices.rs` (create + regenerate paths)
/// **Why**: Fills the static XLSX template with per-invoice data using direct XML
/// surgery on the ZIP contents — same approach as `generate_offer_xlsx`.
///
/// The invoice template has fixed formula rows (E31-E37 = C×D, E38 = SUM, E39 = 19%,
/// E41 = brutto total). We only write B/C/D of the line item rows; all totals are
/// formula-driven and recalculated on open.
///
/// # Parameters
/// - `data` — fully populated `InvoiceData`
///
/// # Returns
/// Raw bytes of a valid `.xlsx` file ready to pass to `convert_xlsx_to_pdf`.
///
/// # Errors
/// - `OfferError::Template` if the template ZIP is corrupt or not valid UTF-8
/// - `OfferError::Template` if ZIP reassembly fails
pub fn generate_invoice_xlsx(data: &InvoiceData) -> Result<Vec<u8>, OfferError> {
    let mut template_zip = ZipArchive::new(Cursor::new(TEMPLATE_BYTES))
        .map_err(|e| OfferError::Template(format!("Failed to read invoice template ZIP: {e}")))?;

    // Read sheet1.xml and apply modifications
    let sheet1_xml = read_zip_entry(&mut template_zip, "xl/worksheets/sheet1.xml")?;
    let sheet1_str = String::from_utf8(sheet1_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml not valid UTF-8: {e}")))?;

    let cell_mods = build_cell_modifications(data);
    let modified_sheet1 = apply_modifications(&sheet1_str, &cell_mods);

    // Read workbook.xml and force recalculation on load
    let workbook_xml = read_zip_entry(&mut template_zip, "xl/workbook.xml")?;
    let workbook_str = String::from_utf8(workbook_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml not valid UTF-8: {e}")))?;
    let modified_workbook = force_recalc_on_load(&workbook_str);

    // Reassemble ZIP: replace sheet1.xml + workbook.xml, keep everything else as-is
    assemble_invoice_xlsx(&mut template_zip, &modified_sheet1, &modified_workbook)
}

// ---------------------------------------------------------------------------
// Cell modification builder
// ---------------------------------------------------------------------------

/// Build the list of cell value changes for this invoice.
///
/// **Why**: Separates "what to change" (domain logic) from "how to change XML"
/// (XML surgery). All knowledge of which cell holds which field lives here.
///
/// Template cell map:
/// - A8:  Customer name
/// - A9:  Customer email
/// - A10: Origin street
/// - A11: Origin postal code + city
/// - C19: Leistungsdatum (service date)
/// - E19: Rechnungsdatum (invoice date) — replaces `=TODAY()` formula
/// - A22: Invoice number heading (merged A22:C22)
/// - A24: Salutation (merged A24:B24)
/// - A27: Auftragsort (merged A27:E28)
/// - B31: Base line item description
/// - C31: Qty = 1
/// - D31: Base netto unit price in EUR
/// - B32–B37: Extra service descriptions (up to 6)
/// - C32–C37: Qty = 1 per extra
/// - D32–D37: Extra netto unit prices in EUR
fn build_cell_modifications(data: &InvoiceData) -> Vec<(String, CellValue)> {
    let mut mods = Vec::new();

    // Customer address block
    mods.push(("A8".into(), CellValue::Text(data.customer_name.clone())));
    mods.push(("A9".into(), CellValue::Text(data.customer_email.clone())));
    mods.push(("A10".into(), CellValue::Text(data.origin_street.clone())));
    mods.push(("A11".into(), CellValue::Text(data.origin_city.clone())));

    // Dates — write as pre-formatted strings to avoid date-serial/style issues
    let service_date_str = data
        .service_date
        .map(|d| d.format("%d.%m.%Y").to_string())
        .unwrap_or_default();
    mods.push(("C19".into(), CellValue::Text(service_date_str)));
    // E19 has =TODAY() formula; replace with actual invoice date text
    mods.push((
        "E19".into(),
        CellValue::Text(data.invoice_date.format("%d.%m.%Y").to_string()),
    ));

    // Invoice number heading (A22, merged A:C, bold 18pt in template)
    mods.push((
        "A22".into(),
        CellValue::Text(format!("Rechnung Nr.{}", data.invoice_number)),
    ));

    // Salutation (A24, merged A:B)
    mods.push(("A24".into(), CellValue::Text(data.salutation.clone())));

    // Job location (A27, merged A:E across rows 27-28)
    mods.push((
        "A27".into(),
        CellValue::Text(format!(
            "Auftragsort:  {}, {}",
            data.origin_street, data.origin_city
        )),
    ));

    // Base line item (row 31)
    let base_description = match &data.invoice_type {
        InvoiceType::Full => format!(
            "Umzugsdienstleistung gemäß Angebot Nr. {}",
            data.offer_number
        ),
        InvoiceType::PartialFirst { percent } => format!(
            "Anzahlung ({}%) — Umzugsdienstleistung gemäß Angebot Nr. {}",
            percent, data.offer_number
        ),
        InvoiceType::PartialFinal => format!(
            "Restbetrag — Umzugsdienstleistung gemäß Angebot Nr. {}",
            data.offer_number
        ),
    };
    let base_netto_euros = data.base_netto_cents as f64 / 100.0;
    mods.push(("B31".into(), CellValue::Text(base_description)));
    mods.push(("C31".into(), CellValue::Number(1.0)));
    mods.push(("D31".into(), CellValue::Number(base_netto_euros)));

    // Extra services (rows 32–37, max 6)
    for (i, extra) in data.extra_services.iter().take(6).enumerate() {
        let row = 32 + i as u32;
        let extra_euros = extra.price_cents as f64 / 100.0;
        mods.push((format!("B{row}"), CellValue::Text(extra.description.clone())));
        mods.push((format!("C{row}"), CellValue::Number(1.0)));
        mods.push((format!("D{row}"), CellValue::Number(extra_euros)));
    }

    mods
}

// ---------------------------------------------------------------------------
// XML modification engine (trimmed from xlsx.rs — invoice-specific)
// ---------------------------------------------------------------------------

/// Internal cell value representation used during XML construction.
enum CellValue {
    /// Plain text, preserves the original template cell style.
    Text(String),
    /// Numeric value, preserves the original template cell style.
    Number(f64),
}

/// Apply cell modifications to `sheet1.xml` and strip stale formula caches.
///
/// **Why**: After writing new values to C/D columns, the structured-reference
/// formulas in column E (E31-E37 = C×D) and the sum rows (E38, E39, E41) have
/// stale `<v>` cached values from the template save. Stripping them forces
/// LibreOffice to recalculate on open so the PDF shows correct totals.
fn apply_modifications(xml: &str, cell_mods: &[(String, CellValue)]) -> String {
    let mut result = xml.to_string();
    for (cell_ref, value) in cell_mods {
        result = set_cell_value(&result, cell_ref, value);
    }
    strip_formula_cached_values(&result)
}

/// Replace the value of a single cell in `sheet1.xml`.
///
/// **Why**: Handles three cases: existing element with children, self-closing
/// element, or cell not present in the template at all (insert it).
/// Preserves the original cell's `s="N"` style attribute so template formatting
/// (currency, date, bold etc.) is kept.
fn set_cell_value(xml: &str, cell_ref: &str, value: &CellValue) -> String {
    let ref_pattern = format!(r#"r="{}""#, cell_ref);

    if let Some(attr_pos) = xml.find(&ref_pattern) {
        let c_start = match xml[..attr_pos].rfind("<c ") {
            Some(pos) => pos,
            None => return xml.to_string(),
        };
        let after_c_start = &xml[c_start..];
        let c_end = match find_cell_end(after_c_start) {
            Some(offset) => c_start + offset,
            None => return xml.to_string(),
        };

        let cell_fragment = &xml[c_start..c_end];
        let style = extract_attribute(cell_fragment, "s");
        let replacement = build_cell_xml(cell_ref, style.as_deref(), value);

        let mut result = String::with_capacity(xml.len());
        result.push_str(&xml[..c_start]);
        result.push_str(&replacement);
        result.push_str(&xml[c_end..]);
        result
    } else {
        insert_cell(xml, cell_ref, value)
    }
}

/// Find the byte offset past the end of a `<c ...>...</c>` or `<c .../>` element.
fn find_cell_end(fragment: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = 0;
    let bytes = fragment.as_bytes();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if fragment[i..].starts_with("<c ") || fragment[i..].starts_with("<c>") {
                if depth == 0 {
                    if let Some(gt) = fragment[i..].find('>') {
                        if bytes[i + gt - 1] == b'/' {
                            return Some(i + gt + 1);
                        }
                        depth = 1;
                        i = i + gt + 1;
                        continue;
                    }
                }
                depth += 1;
            } else if fragment[i..].starts_with("</c>") {
                if depth == 1 {
                    return Some(i + 4);
                }
                depth -= 1;
            }
        }
        i += 1;
    }
    None
}

/// Build a complete `<c>` XML element string.
fn build_cell_xml(cell_ref: &str, style: Option<&str>, value: &CellValue) -> String {
    let s_attr = match style {
        Some(s) => format!(r#" s="{}""#, s),
        None => String::new(),
    };
    match value {
        CellValue::Text(text) => {
            let escaped = xml_escape(text);
            format!(
                r#"<c r="{}"{} t="inlineStr"><is><t>{}</t></is></c>"#,
                cell_ref, s_attr, escaped
            )
        }
        CellValue::Number(n) => {
            let formatted = format_number(*n);
            format!(r#"<c r="{}"{} t="n"><v>{}</v></c>"#, cell_ref, s_attr, formatted)
        }
    }
}

/// Insert a new `<c>` element into the correct row (or create the row).
fn insert_cell(xml: &str, cell_ref: &str, value: &CellValue) -> String {
    let row_num = extract_row_number(cell_ref);
    let row_pattern = format!(r#"r="{}""#, row_num);

    let mut search_from = 0;
    while let Some(pos) = xml[search_from..].find(&row_pattern) {
        let abs_pos = search_from + pos;
        let before = &xml[..abs_pos];
        if let Some(row_start) = before.rfind("<row ") {
            let after_match = abs_pos + row_pattern.len();
            if after_match < xml.len() {
                let next_char = xml.as_bytes()[after_match];
                if next_char == b'"' || next_char == b' ' || next_char == b'>' {
                    let row_fragment = &xml[row_start..];
                    if let Some(end_row) = row_fragment.find("</row>") {
                        let insert_pos = row_start + end_row;
                        let cell_xml = build_cell_xml(cell_ref, None, value);
                        let mut result = String::with_capacity(xml.len() + cell_xml.len());
                        result.push_str(&xml[..insert_pos]);
                        result.push_str(&cell_xml);
                        result.push_str(&xml[insert_pos..]);
                        return result;
                    }
                }
            }
        }
        search_from = abs_pos + 1;
    }

    // Row doesn't exist — create it
    if let Some(sd_end) = xml.find("</sheetData>") {
        let cell_xml = build_cell_xml(cell_ref, None, value);
        let row_xml = format!(r#"<row r="{}">{}</row>"#, row_num, cell_xml);
        let mut result = String::with_capacity(xml.len() + row_xml.len());
        result.push_str(&xml[..sd_end]);
        result.push_str(&row_xml);
        result.push_str(&xml[sd_end..]);
        return result;
    }

    xml.to_string()
}

/// Remove stale `<v>` cached values from formula cells so LibreOffice recalculates.
fn strip_formula_cached_values(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len());
    let mut remaining = xml;

    while let Some(f_start) = remaining.find("<f") {
        // Find the enclosing <c> element
        let before_f = &remaining[..f_start];
        if let Some(c_start_rel) = before_f.rfind("<c ") {
            result.push_str(&remaining[..c_start_rel]);

            let cell_fragment = &remaining[c_start_rel..];
            if let Some(cell_end) = find_cell_end(cell_fragment) {
                let cell_xml = &cell_fragment[..cell_end];
                // Remove <v>...</v> and <v/> from the cell
                let cleaned = remove_v_elements(cell_xml);
                result.push_str(&cleaned);
                remaining = &remaining[c_start_rel + cell_end..];
            } else {
                result.push_str(&remaining[..f_start + 2]);
                remaining = &remaining[f_start + 2..];
            }
        } else {
            result.push_str(&remaining[..f_start + 2]);
            remaining = &remaining[f_start + 2..];
        }
    }
    result.push_str(remaining);
    result
}

/// Remove all `<v>…</v>` and `<v/>` elements from a cell XML fragment.
fn remove_v_elements(cell_xml: &str) -> String {
    let mut result = cell_xml.to_string();
    // Remove <v>...</v>
    while let Some(v_start) = result.find("<v>") {
        if let Some(v_end) = result[v_start..].find("</v>") {
            result = format!("{}{}", &result[..v_start], &result[v_start + v_end + 4..]);
        } else {
            break;
        }
    }
    // Remove self-closing <v/>
    result = result.replace("<v/>", "");
    result
}

/// Add `fullCalcOnLoad="1"` to `<calcPr>` in `workbook.xml` so LibreOffice
/// recalculates all formulas on first open.
fn force_recalc_on_load(workbook_xml: &str) -> String {
    // If already present, ensure it's set to 1
    if workbook_xml.contains(r#"fullCalcOnLoad="0""#) {
        return workbook_xml.replace(r#"fullCalcOnLoad="0""#, r#"fullCalcOnLoad="1""#);
    }
    if workbook_xml.contains("fullCalcOnLoad=") {
        return workbook_xml.to_string(); // already set to 1 or other truthy value
    }
    // Inject into existing <calcPr ...> tag
    if let Some(pos) = workbook_xml.find("<calcPr") {
        let after = &workbook_xml[pos..];
        if let Some(gt) = after.find('>') {
            let insert_at = pos + gt;
            let mut result = workbook_xml.to_string();
            result.insert_str(insert_at, r#" fullCalcOnLoad="1""#);
            return result;
        }
    }
    workbook_xml.to_string()
}

// ---------------------------------------------------------------------------
// ZIP utilities
// ---------------------------------------------------------------------------

/// Read a named entry from an open ZIP archive into a byte vector.
fn read_zip_entry(
    zip: &mut ZipArchive<Cursor<&'static [u8]>>,
    name: &str,
) -> Result<Vec<u8>, OfferError> {
    let mut entry = zip
        .by_name(name)
        .map_err(|e| OfferError::Template(format!("Entry '{name}' not found in template: {e}")))?;
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| OfferError::Template(format!("Failed to read '{name}': {e}")))?;
    Ok(buf)
}

/// Reassemble the invoice XLSX ZIP: swap in modified sheet1.xml and workbook.xml,
/// copy all other entries unchanged.
fn assemble_invoice_xlsx(
    template_zip: &mut ZipArchive<Cursor<&'static [u8]>>,
    modified_sheet1: &str,
    modified_workbook: &str,
) -> Result<Vec<u8>, OfferError> {
    let buf: Vec<u8> = Vec::new();
    let cursor = Cursor::new(buf);
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for i in 0..template_zip.len() {
        let mut entry = template_zip
            .by_index(i)
            .map_err(|e| OfferError::Template(format!("Failed to read ZIP entry {i}: {e}")))?;
        let name = entry.name().to_string();

        writer
            .start_file(&name, options)
            .map_err(|e| OfferError::Template(format!("Failed to start ZIP entry '{name}': {e}")))?;

        match name.as_str() {
            "xl/worksheets/sheet1.xml" => {
                writer
                    .write_all(modified_sheet1.as_bytes())
                    .map_err(|e| OfferError::Template(format!("Failed to write sheet1.xml: {e}")))?;
            }
            "xl/workbook.xml" => {
                writer
                    .write_all(modified_workbook.as_bytes())
                    .map_err(|e| OfferError::Template(format!("Failed to write workbook.xml: {e}")))?;
            }
            _ => {
                let mut content = Vec::new();
                entry
                    .read_to_end(&mut content)
                    .map_err(|e| OfferError::Template(format!("Failed to read '{name}': {e}")))?;
                writer
                    .write_all(&content)
                    .map_err(|e| OfferError::Template(format!("Failed to write '{name}': {e}")))?;
            }
        }
    }

    let finished = writer
        .finish()
        .map_err(|e| OfferError::Template(format!("Failed to finalise invoice ZIP: {e}")))?;
    Ok(finished.into_inner())
}

// ---------------------------------------------------------------------------
// String utilities
// ---------------------------------------------------------------------------

/// Escape XML special characters in a text value.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Format a float for an XLSX `<v>` element (no scientific notation, trim trailing zeros).
fn format_number(n: f64) -> String {
    if n == n.floor() && n.abs() < 1e15 {
        format!("{:.0}", n)
    } else {
        let s = format!("{:.10}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Extract an XML attribute value, e.g. `extract_attribute(r#"<c r="A1" s="5">"#, "s")` → `Some("5")`.
fn extract_attribute(xml: &str, attr: &str) -> Option<String> {
    let search = format!(r#"{}=""#, attr);
    let start = xml.find(&search)? + search.len();
    let end = xml[start..].find('"')?;
    Some(xml[start..start + end].to_string())
}

/// Extract the row number from an Excel cell reference, e.g. `"D31"` → `31`.
fn extract_row_number(cell_ref: &str) -> u32 {
    cell_ref
        .chars()
        .skip_while(|c| c.is_alphabetic())
        .collect::<String>()
        .parse()
        .unwrap_or(1)
}
