//! Direct XLSX template manipulation — no umya-spreadsheet.
//!
//! XLSX = ZIP of XML files. We open the template ZIP, surgically modify only
//! the cell values we need, hide unused line-item rows, add an items sheet,
//! and re-ZIP. All drawings, media, styles, column widths, page setup, and
//! merge cells are preserved bit-for-bit from the template.

use crate::OfferError;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// The original template XLSX ZIP — embedded at compile time.
const TEMPLATE_BYTES: &[u8] = include_bytes!("../../../templates/offer_template.xlsx");

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferData {
    pub offer_number: String,
    pub date: chrono::NaiveDate,
    pub valid_until: Option<chrono::NaiveDate>,
    pub customer_salutation: String,
    pub customer_name: String,
    pub customer_street: String,
    pub customer_city: String,
    pub customer_phone: String,
    pub customer_email: String,
    pub greeting: String,
    pub moving_date: String,
    pub origin_street: String,
    pub origin_city: String,
    pub origin_floor_info: String,
    pub dest_street: String,
    pub dest_city: String,
    pub dest_floor_info: String,
    pub volume_m3: f64,
    pub persons: u32,
    pub estimated_hours: f64,
    pub rate_per_person_hour: f64,
    pub line_items: Vec<OfferLineItem>,
    pub detected_items: Vec<DetectedItemRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferLineItem {
    pub row: u32,
    pub description: Option<String>,
    pub quantity: f64,
    pub unit_price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedItemRow {
    pub name: String,
    pub volume_m3: f64,
    pub dimensions: Option<String>,
    pub confidence: f64,
    #[serde(default)]
    pub german_name: Option<String>,
    #[serde(default)]
    pub re_value: Option<f64>,
    #[serde(default)]
    pub volume_source: Option<String>,
    #[serde(default)]
    pub crop_s3_key: Option<String>,
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    #[serde(default)]
    pub source_image_urls: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Cell value types for modifications
// ---------------------------------------------------------------------------

enum CellValue {
    Text(String),
    Number(f64),
    /// Number with an explicit style index (used when inserting into a new cell).
    StyledNumber(f64, &'static str),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate an XLSX offer from template + data. Returns the final XLSX bytes.
pub fn generate_offer_xlsx(data: &OfferData) -> Result<Vec<u8>, OfferError> {
    let mut template_zip = ZipArchive::new(Cursor::new(TEMPLATE_BYTES))
        .map_err(|e| OfferError::Template(format!("Failed to read template ZIP: {e}")))?;

    // Build modifications
    let cell_mods = build_cell_modifications(data);
    let hidden_rows = compute_hidden_rows(data);

    // Read and modify sheet1.xml
    let sheet1_xml = read_zip_entry(&mut template_zip, "xl/worksheets/sheet1.xml")?;
    let sheet1_str = String::from_utf8(sheet1_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml is not valid UTF-8: {e}")))?;
    let mut modified_sheet1 = apply_modifications(&sheet1_str, &cell_mods, &hidden_rows);

    // Remove hyperlinks section — we want plain text, not clickable links
    modified_sheet1 = strip_hyperlinks(&modified_sheet1);

    // Read and fix workbook.xml (print area + possibly add items sheet reference)
    let workbook_xml = read_zip_entry(&mut template_zip, "xl/workbook.xml")?;
    let workbook_str = String::from_utf8(workbook_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml is not valid UTF-8: {e}")))?;

    let has_items = !data.detected_items.is_empty();
    let modified_workbook = modify_workbook(&workbook_str, has_items);

    // Build items sheet if needed
    let items_sheet_xml = if has_items {
        Some(build_items_sheet_xml(&data.detected_items))
    } else {
        None
    };

    // Read and modify Content_Types and rels if we're adding items sheet
    let content_types_xml = read_zip_entry(&mut template_zip, "[Content_Types].xml")?;
    let content_types_str = String::from_utf8(content_types_xml)
        .map_err(|e| OfferError::Template(format!("Content_Types.xml not valid UTF-8: {e}")))?;
    let modified_content_types = if has_items {
        add_sheet2_content_type(&content_types_str)
    } else {
        content_types_str
    };

    let rels_xml = read_zip_entry(&mut template_zip, "xl/_rels/workbook.xml.rels")?;
    let rels_str = String::from_utf8(rels_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml.rels not valid UTF-8: {e}")))?;
    let modified_rels = if has_items {
        add_sheet2_relationship(&rels_str)
    } else {
        rels_str
    };

    // Remove hyperlink relationships from sheet1 rels (keep drawing rel)
    let sheet1_rels_xml =
        read_zip_entry(&mut template_zip, "xl/worksheets/_rels/sheet1.xml.rels")?;
    let sheet1_rels_str = String::from_utf8(sheet1_rels_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml.rels not valid UTF-8: {e}")))?;
    let modified_sheet1_rels = strip_hyperlink_rels(&sheet1_rels_str);

    // Fix date format in styles.xml: m/d/yyyy → dd.mm.yyyy (German)
    let styles_xml = read_zip_entry(&mut template_zip, "xl/styles.xml")?;
    let styles_str = String::from_utf8(styles_xml)
        .map_err(|e| OfferError::Template(format!("styles.xml not valid UTF-8: {e}")))?;
    let modified_styles = styles_str.replace(
        r#"formatCode="m/d/yyyy""#,
        r#"formatCode="dd.mm.yyyy""#,
    );

    // Assemble output ZIP
    assemble_xlsx(
        &mut template_zip,
        &modified_sheet1,
        &modified_workbook,
        &modified_content_types,
        &modified_rels,
        &modified_sheet1_rels,
        &modified_styles,
        items_sheet_xml.as_deref(),
    )
}

// ---------------------------------------------------------------------------
// Build cell modifications from OfferData
// ---------------------------------------------------------------------------

fn build_cell_modifications(data: &OfferData) -> Vec<(String, CellValue)> {
    let mut mods = Vec::new();

    // Customer address block
    mods.push(("A8".into(), CellValue::Text(data.customer_salutation.clone())));
    mods.push(("A9".into(), CellValue::Text(data.customer_name.clone())));
    mods.push(("A10".into(), CellValue::Text(data.customer_street.clone())));
    mods.push(("A11".into(), CellValue::Text(data.customer_city.clone())));

    // Clear the old TODAY() formula in G14 (overlaps with company info text box).
    mods.push(("G14".into(), CellValue::Text(String::new())));
    // Date in G15 — below the text box. Style 10 = dd.mm.yyyy date format.
    mods.push(("G15".into(), CellValue::StyledNumber(date_to_excel_serial(data.date), "10")));

    // Title
    mods.push((
        "A16".into(),
        CellValue::Text(format!(
            "Unverbindlicher Kostenvoranschlag {}",
            data.offer_number
        )),
    ));

    // Moving date & contact
    mods.push(("B17".into(), CellValue::Text(data.moving_date.clone())));
    mods.push(("B18".into(), CellValue::Text(data.customer_phone.clone())));
    mods.push(("F18".into(), CellValue::Text(data.customer_email.clone())));

    // Greeting
    mods.push(("A20".into(), CellValue::Text(data.greeting.clone())));

    // Origin address
    mods.push(("A26".into(), CellValue::Text(data.origin_street.clone())));
    mods.push(("A27".into(), CellValue::Text(data.origin_city.clone())));
    mods.push(("A28".into(), CellValue::Text(data.origin_floor_info.clone())));

    // Destination address
    mods.push(("F26".into(), CellValue::Text(data.dest_street.clone())));
    mods.push(("F27".into(), CellValue::Text(data.dest_city.clone())));
    mods.push(("F28".into(), CellValue::Text(data.dest_floor_info.clone())));

    // Volume description
    mods.push((
        "A29".into(),
        CellValue::Text(format!("Umzugspauschale {:.1} m³", data.volume_m3)),
    ));

    // Clear all line item quantities and prices (rows 31-42, except 38=labor)
    for row in 31..=42 {
        if row == 38 {
            continue;
        }
        mods.push((format!("E{row}"), CellValue::Number(0.0)));
        mods.push((format!("F{row}"), CellValue::Number(0.0)));
    }

    // Fill in actual line items
    for item in &data.line_items {
        mods.push((format!("E{}", item.row), CellValue::Number(item.quantity)));
        mods.push((format!("F{}", item.row), CellValue::Number(item.unit_price)));
        if let Some(desc) = &item.description {
            mods.push((format!("D{}", item.row), CellValue::Text(desc.clone())));
        }
    }

    // Labor line (row 38)
    mods.push((
        "D38".into(),
        CellValue::Text(format!("{} Umzugshelfer", data.persons)),
    ));
    mods.push(("E38".into(), CellValue::Number(data.estimated_hours)));
    mods.push(("F38".into(), CellValue::Number(data.rate_per_person_hour)));

    // Persons count in J50 (used by G38 formula: =E38*F38*J50)
    mods.push(("J50".into(), CellValue::Number(data.persons as f64)));

    mods
}

/// Determine which line-item rows (31-42) should be hidden.
/// A row is hidden if its quantity is zero after applying modifications.
fn compute_hidden_rows(data: &OfferData) -> Vec<u32> {
    let active_rows: Vec<u32> = data.line_items.iter().map(|li| li.row).collect();
    let mut hidden = Vec::new();
    for row in 31..=42 {
        if row == 38 {
            // Labor row: always visible (has hours/rate)
            continue;
        }
        if !active_rows.contains(&row) {
            hidden.push(row);
        }
    }
    hidden
}

// ---------------------------------------------------------------------------
// XML modification engine
// ---------------------------------------------------------------------------

/// Apply cell modifications and row hiding to the sheet1 XML.
/// Uses targeted string surgery — the template XML has a known, predictable structure.
fn apply_modifications(
    xml: &str,
    cell_mods: &[(String, CellValue)],
    hidden_rows: &[u32],
) -> String {
    let mut result = xml.to_string();

    // 1. Apply cell modifications
    for (cell_ref, value) in cell_mods {
        result = set_cell_value(&result, cell_ref, value);
    }

    // 2. Hide rows
    for &row_num in hidden_rows {
        result = hide_row(&result, row_num);
    }

    // 3. Strip cached values from formula cells so LibreOffice must recalculate.
    //    Template has stale <v> values that won't match our new inputs.
    result = strip_formula_cached_values(&result);

    result
}

/// Set a cell's value in the sheet XML.
/// Handles three cases: cell exists with children, cell is self-closing, cell doesn't exist.
fn set_cell_value(xml: &str, cell_ref: &str, value: &CellValue) -> String {
    let ref_pattern = format!(r#"r="{}""#, cell_ref);

    if let Some(attr_pos) = xml.find(&ref_pattern) {
        // Find the start of the <c element containing this attribute
        let c_start = match xml[..attr_pos].rfind("<c ") {
            Some(pos) => pos,
            None => return xml.to_string(),
        };

        // Find the end of this cell element
        let after_c_start = &xml[c_start..];
        let c_end = if let Some(sc_pos) = find_cell_end(after_c_start) {
            c_start + sc_pos
        } else {
            return xml.to_string();
        };

        // Extract the style attribute (s="N") from the original cell
        let cell_fragment = &xml[c_start..c_end];
        let style = extract_attribute(cell_fragment, "s");

        // Build replacement cell
        let replacement = build_cell_xml(cell_ref, style.as_deref(), value);

        let mut result = String::with_capacity(xml.len());
        result.push_str(&xml[..c_start]);
        result.push_str(&replacement);
        result.push_str(&xml[c_end..]);
        result
    } else {
        // Cell doesn't exist — insert it into the correct row
        insert_cell(xml, cell_ref, value)
    }
}

/// Find the end of a <c ...>...</c> or <c .../> element.
/// Returns the byte offset PAST the closing tag, relative to the input.
fn find_cell_end(fragment: &str) -> Option<usize> {
    // Check for self-closing <c ... />
    let mut depth = 0;
    let mut i = 0;
    let bytes = fragment.as_bytes();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if fragment[i..].starts_with("<c ") || fragment[i..].starts_with("<c>") {
                if depth == 0 {
                    // This is our opening tag — check for self-closing
                    if let Some(gt) = fragment[i..].find('>') {
                        if bytes[i + gt - 1] == b'/' {
                            // Self-closing: <c ... />
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

/// Build a <c> element XML string.
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
            // Format: avoid scientific notation, trim trailing zeros
            let formatted = format_number(*n);
            format!(r#"<c r="{}"{} t="n"><v>{}</v></c>"#, cell_ref, s_attr, formatted)
        }
        CellValue::StyledNumber(n, forced_style) => {
            // Like Number but always applies the given style, even when inserting new cells.
            let formatted = format_number(*n);
            format!(
                r#"<c r="{}" s="{}" t="n"><v>{}</v></c>"#,
                cell_ref, forced_style, formatted
            )
        }
    }
}

/// Insert a cell into an existing row (for cells that don't exist in the template).
fn insert_cell(xml: &str, cell_ref: &str, value: &CellValue) -> String {
    let row_num = extract_row_number(cell_ref);
    let row_pattern = format!(r#"r="{}""#, row_num);

    // Find the <row> element for this row number
    // We need to find <row r="N" specifically (not r="N1" or r="N2")
    let mut search_from = 0;
    while let Some(pos) = xml[search_from..].find(&row_pattern) {
        let abs_pos = search_from + pos;
        // Make sure this is inside a <row element and the match is exact
        let before = &xml[..abs_pos];
        if let Some(row_start) = before.rfind("<row ") {
            // Verify the row number is exact (followed by " not a digit)
            let after_match = abs_pos + row_pattern.len();
            if after_match < xml.len() {
                let next_char = xml.as_bytes()[after_match];
                if next_char == b'"' || next_char == b' ' || next_char == b'>' {
                    // Found the right row. Insert before </row>
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

    // Row doesn't exist — create it inside <sheetData>
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

/// Add or set hidden="1" attribute on a <row> element.
fn hide_row(xml: &str, row_num: u32) -> String {
    let row_r = format!(r#"<row r="{}""#, row_num);
    if let Some(pos) = xml.find(&row_r) {
        let after = &xml[pos..];
        if let Some(gt_offset) = after.find('>') {
            let gt_pos = pos + gt_offset;
            let tag_content = &xml[pos..gt_pos];

            // If hidden="false" exists, replace it with hidden="true"
            if let Some(h_pos) = tag_content.find(r#"hidden="false""#) {
                let abs_h = pos + h_pos;
                let mut result = String::with_capacity(xml.len());
                result.push_str(&xml[..abs_h]);
                result.push_str(r#"hidden="true""#);
                result.push_str(&xml[abs_h + r#"hidden="false""#.len()..]);
                return result;
            }

            // If no hidden attribute, add it
            if !tag_content.contains("hidden=") {
                let mut result = String::with_capacity(xml.len() + 14);
                result.push_str(&xml[..gt_pos]);
                result.push_str(r#" hidden="true""#);
                result.push_str(&xml[gt_pos..]);
                return result;
            }
        }
    }
    xml.to_string()
}

/// Remove cached <v>...</v> values from cells that have formulas (<f>...</f>).
/// This forces LibreOffice to recalculate all formulas on open.
fn strip_formula_cached_values(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len());
    let mut pos = 0;

    while pos < xml.len() {
        // Find next cell opening tag
        if let Some(c_offset) = xml[pos..].find("<c ") {
            let abs_c_start = pos + c_offset;

            // Find the end of this cell
            if let Some(c_len) = find_cell_end(&xml[abs_c_start..]) {
                let cell = &xml[abs_c_start..abs_c_start + c_len];

                if cell.contains("<f") {
                    // Formula cell: strip cached <v>...</v>
                    let cleaned = strip_v_element(cell);
                    result.push_str(&xml[pos..abs_c_start]);
                    result.push_str(&cleaned);
                } else {
                    // Not a formula cell: copy as-is
                    result.push_str(&xml[pos..abs_c_start + c_len]);
                }
                pos = abs_c_start + c_len;
            } else {
                // Can't find cell end — copy rest and stop
                result.push_str(&xml[pos..]);
                return result;
            }
        } else {
            // No more cells — copy remainder
            result.push_str(&xml[pos..]);
            break;
        }
    }

    result
}

/// Remove <v>...</v> element from a cell XML fragment.
fn strip_v_element(cell: &str) -> String {
    if let Some(v_start) = cell.find("<v>") {
        if let Some(v_end) = cell[v_start..].find("</v>") {
            let mut result = String::with_capacity(cell.len());
            result.push_str(&cell[..v_start]);
            result.push_str(&cell[v_start + v_end + 4..]);
            return result;
        }
    }
    // Also handle <v/> (empty cached value)
    if let Some(v_start) = cell.find("<v/>") {
        let mut result = String::with_capacity(cell.len());
        result.push_str(&cell[..v_start]);
        result.push_str(&cell[v_start + 4..]);
        return result;
    }
    cell.to_string()
}

// ---------------------------------------------------------------------------
// Workbook modification
// ---------------------------------------------------------------------------

/// Fix the print area (A:H only), force recalculation, and optionally add items sheet reference.
fn modify_workbook(xml: &str, add_items_sheet: bool) -> String {
    let mut result = xml.to_string();

    // Fix print area: replace dual-range with A:H only
    result = fix_print_area(&result);

    // Force full recalculation on load (so formulas pick up new cell values)
    result = force_recalc(&result);

    // Add items sheet if needed
    if add_items_sheet {
        result = add_items_sheet_to_workbook(&result);
    }

    result
}

/// Add fullCalcOnLoad="true" to <calcPr> so LibreOffice recalculates all formulas.
fn force_recalc(xml: &str) -> String {
    if let Some(pos) = xml.find("<calcPr") {
        if let Some(gt) = xml[pos..].find("/>") {
            let tag_end = pos + gt;
            // Insert fullCalcOnLoad before the closing />
            let mut result = String::with_capacity(xml.len() + 25);
            result.push_str(&xml[..tag_end]);
            result.push_str(r#" fullCalcOnLoad="true""#);
            result.push_str(&xml[tag_end..]);
            return result;
        }
    }
    xml.to_string()
}

/// Replace the dual-range print area with A:H only.
fn fix_print_area(xml: &str) -> String {
    if let Some(start) = xml.find("_xlnm.Print_Area") {
        if let Some(content_start) = xml[start..].find('>') {
            let abs_content_start = start + content_start + 1;
            if let Some(end_tag) = xml[abs_content_start..].find("</definedName>") {
                let abs_end = abs_content_start + end_tag;
                let mut result = String::with_capacity(xml.len());
                result.push_str(&xml[..abs_content_start]);
                result.push_str("Tabelle1!$A$1:$H$120");
                result.push_str(&xml[abs_end..]);
                return result;
            }
        }
    }
    xml.to_string()
}

/// Add a <sheet> element for the items sheet.
fn add_items_sheet_to_workbook(xml: &str) -> String {
    // Insert before </sheets>
    let marker = "</sheets>";
    if let Some(pos) = xml.find(marker) {
        let sheet_xml = r#"<sheet name="Erfasste Gegenstände" sheetId="2" r:id="rId5"/>"#;
        let mut result = String::with_capacity(xml.len() + sheet_xml.len());
        result.push_str(&xml[..pos]);
        result.push_str(sheet_xml);
        result.push_str(&xml[pos..]);
        return result;
    }
    xml.to_string()
}

// ---------------------------------------------------------------------------
// Items sheet builder — uses styles from the template's styles.xml
// ---------------------------------------------------------------------------

// Style indices from the template's xl/styles.xml:
// 47 = bold 13pt, orange fill, full border, center/v-top/wrap  (header)
// 17 = full border, fill3, left                                 (data row A text)
// 18 = full border, fill3, center/v-center                      (data row A number)
// 21 = full border, fill4, left                                 (data row B text)
// 22 = full border, fill4, center/v-center                      (data row B number)
// 75 = full border, no fill, general                            (total row)

fn build_items_sheet_xml(items: &[DetectedItemRow]) -> String {
    let mut xml = String::with_capacity(8192);

    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#);
    xml.push_str(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#);

    xml.push_str(r#"<sheetPr><pageSetUpPr fitToPage="1"/></sheetPr>"#);

    let last_row = items.len() + 2; // header + items + total
    xml.push_str(&format!(r#"<dimension ref="A1:D{}"/>"#, last_row));

    // 4 columns: Nr, Gegenstand, Bezeichnung (DE), Volumen
    xml.push_str(r#"<cols>"#);
    xml.push_str(r#"<col min="1" max="1" width="6" customWidth="1"/>"#);
    xml.push_str(r#"<col min="2" max="2" width="40" customWidth="1"/>"#);
    xml.push_str(r#"<col min="3" max="3" width="36" customWidth="1"/>"#);
    xml.push_str(r#"<col min="4" max="4" width="16" customWidth="1"/>"#);
    xml.push_str(r#"</cols>"#);

    xml.push_str(r#"<sheetFormatPr defaultRowHeight="15"/>"#);
    xml.push_str(r#"<sheetData>"#);

    // Header row
    xml.push_str(r#"<row r="1" ht="18">"#);
    for (col, header) in [("A", "Nr."), ("B", "Gegenstand"), ("C", "Bezeichnung (DE)"), ("D", "Volumen (m³)")] {
        xml.push_str(&format!(
            r#"<c r="{}1" s="47" t="inlineStr"><is><t>{}</t></is></c>"#,
            col, xml_escape(header)
        ));
    }
    xml.push_str(r#"</row>"#);

    // Data rows
    let mut total_volume = 0.0;
    for (i, item) in items.iter().enumerate() {
        let row = i + 2;
        let is_odd = i % 2 == 1;
        let s_text = if is_odd { "21" } else { "17" };
        let s_num = if is_odd { "22" } else { "18" };

        xml.push_str(&format!(r#"<row r="{}">"#, row));

        xml.push_str(&format!(
            r#"<c r="A{}" s="{}" t="n"><v>{}</v></c>"#,
            row, s_num, i + 1
        ));
        xml.push_str(&format!(
            r#"<c r="B{}" s="{}" t="inlineStr"><is><t>{}</t></is></c>"#,
            row, s_text, xml_escape(&item.name)
        ));

        let german = item.german_name.as_deref().unwrap_or("");
        xml.push_str(&format!(
            r#"<c r="C{}" s="{}" t="inlineStr"><is><t>{}</t></is></c>"#,
            row, s_text, xml_escape(german)
        ));

        xml.push_str(&format!(
            r#"<c r="D{}" s="{}" t="inlineStr"><is><t>{:.2} m³</t></is></c>"#,
            row, s_num, item.volume_m3
        ));

        xml.push_str(r#"</row>"#);
        total_volume += item.volume_m3;
    }

    // Total row
    let total_row = items.len() + 2;
    xml.push_str(&format!(r#"<row r="{}" ht="18">"#, total_row));
    xml.push_str(&format!(
        r#"<c r="A{}" s="47" t="inlineStr"><is><t></t></is></c>"#,
        total_row
    ));
    xml.push_str(&format!(
        r#"<c r="B{}" s="47" t="inlineStr"><is><t>Gesamt</t></is></c>"#,
        total_row
    ));
    xml.push_str(&format!(
        r#"<c r="C{}" s="47" t="inlineStr"><is><t>{} Gegenstände</t></is></c>"#,
        total_row, items.len()
    ));
    xml.push_str(&format!(
        r#"<c r="D{}" s="47" t="inlineStr"><is><t>{:.2} m³</t></is></c>"#,
        total_row, total_volume
    ));
    xml.push_str(r#"</row>"#);

    xml.push_str(r#"</sheetData>"#);

    xml.push_str(r#"<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>"#);
    xml.push_str(r#"<pageSetup paperSize="9" orientation="portrait" fitToWidth="1" fitToHeight="0"/>"#);

    xml.push_str(r#"</worksheet>"#);

    xml
}

// ---------------------------------------------------------------------------
// Content_Types and relationship updates
// ---------------------------------------------------------------------------

fn add_sheet2_content_type(xml: &str) -> String {
    let new_entry =
        r#"<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#;
    if let Some(pos) = xml.find("</Types>") {
        let mut result = String::with_capacity(xml.len() + new_entry.len());
        result.push_str(&xml[..pos]);
        result.push_str(new_entry);
        result.push_str(&xml[pos..]);
        return result;
    }
    xml.to_string()
}

fn add_sheet2_relationship(xml: &str) -> String {
    let new_rel = r#"<Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>"#;
    if let Some(pos) = xml.find("</Relationships>") {
        let mut result = String::with_capacity(xml.len() + new_rel.len());
        result.push_str(&xml[..pos]);
        result.push_str(new_rel);
        result.push_str(&xml[pos..]);
        return result;
    }
    xml.to_string()
}

/// Remove the `<hyperlinks>...</hyperlinks>` section from sheet1.xml.
/// This converts linked cells (E-Mail, email address) to plain text.
fn strip_hyperlinks(xml: &str) -> String {
    if let Some(start) = xml.find("<hyperlinks>") {
        if let Some(end_tag) = xml[start..].find("</hyperlinks>") {
            let abs_end = start + end_tag + "</hyperlinks>".len();
            let mut result = String::with_capacity(xml.len());
            result.push_str(&xml[..start]);
            result.push_str(&xml[abs_end..]);
            return result;
        }
    }
    xml.to_string()
}

/// Remove hyperlink Relationship entries from sheet1.xml.rels, keeping the drawing rel.
fn strip_hyperlink_rels(xml: &str) -> String {
    let mut result = String::with_capacity(xml.len());
    let mut pos = 0;
    while pos < xml.len() {
        if let Some(rel_start) = xml[pos..].find("<Relationship ") {
            let abs_start = pos + rel_start;
            // Find the end of this element
            if let Some(rel_end) = xml[abs_start..].find("/>") {
                let abs_end = abs_start + rel_end + 2;
                let fragment = &xml[abs_start..abs_end];
                if fragment.contains("hyperlink") {
                    // Skip this hyperlink relationship
                    result.push_str(&xml[pos..abs_start]);
                    pos = abs_end;
                    continue;
                }
            }
            // Not a hyperlink — keep it
            let next = abs_start + "<Relationship ".len();
            result.push_str(&xml[pos..next]);
            pos = next;
        } else {
            result.push_str(&xml[pos..]);
            break;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// ZIP assembly
// ---------------------------------------------------------------------------

fn assemble_xlsx(
    template_zip: &mut ZipArchive<Cursor<&[u8]>>,
    sheet1_xml: &str,
    workbook_xml: &str,
    content_types_xml: &str,
    rels_xml: &str,
    sheet1_rels_xml: &str,
    styles_xml: &str,
    items_sheet_xml: Option<&str>,
) -> Result<Vec<u8>, OfferError> {
    let mut output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(&mut output);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // Replacements map: files we provide modified versions of
    let replacements: Vec<(&str, &str)> = vec![
        ("xl/worksheets/sheet1.xml", sheet1_xml),
        ("xl/workbook.xml", workbook_xml),
        ("[Content_Types].xml", content_types_xml),
        ("xl/_rels/workbook.xml.rels", rels_xml),
        ("xl/worksheets/_rels/sheet1.xml.rels", sheet1_rels_xml),
        ("xl/styles.xml", styles_xml),
    ];

    // Copy all template files, replacing modified ones
    for i in 0..template_zip.len() {
        let mut file = template_zip.by_index(i).map_err(|e| {
            OfferError::Template(format!("Failed to read template entry {i}: {e}"))
        })?;
        let name = file.name().to_string();

        if let Some((_, content)) = replacements.iter().find(|(n, _)| *n == name) {
            writer.start_file(&name, options).map_err(map_zip)?;
            writer.write_all(content.as_bytes()).map_err(map_io)?;
        } else {
            // Copy from template unchanged
            let mut data = Vec::new();
            file.read_to_end(&mut data).map_err(|e| {
                OfferError::Template(format!("Failed to read template entry {name}: {e}"))
            })?;
            writer.start_file(&name, options).map_err(map_zip)?;
            writer.write_all(&data).map_err(map_io)?;
        }
    }

    // Add items sheet if present
    if let Some(items_xml) = items_sheet_xml {
        writer
            .start_file("xl/worksheets/sheet2.xml", options)
            .map_err(map_zip)?;
        writer.write_all(items_xml.as_bytes()).map_err(map_io)?;
    }

    writer.finish().map_err(map_zip)?;
    Ok(output.into_inner())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_zip_entry(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, OfferError> {
    let mut file = archive.by_name(name).map_err(|e| {
        OfferError::Template(format!("ZIP entry '{name}' not found: {e}"))
    })?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).map_err(|e| {
        OfferError::Template(format!("Failed to read ZIP entry '{name}': {e}"))
    })?;
    Ok(data)
}

/// Extract an XML attribute value from a tag fragment.
fn extract_attribute(tag: &str, attr_name: &str) -> Option<String> {
    let pattern = format!(r#"{}=""#, attr_name);
    if let Some(start) = tag.find(&pattern) {
        let value_start = start + pattern.len();
        if let Some(end) = tag[value_start..].find('"') {
            return Some(tag[value_start..value_start + end].to_string());
        }
    }
    None
}

/// Extract the row number from a cell reference like "A8" → 8, "AB123" → 123.
fn extract_row_number(cell_ref: &str) -> u32 {
    let num_start = cell_ref.find(|c: char| c.is_ascii_digit()).unwrap_or(0);
    cell_ref[num_start..].parse().unwrap_or(1)
}

/// Convert a chrono::NaiveDate to an Excel serial date number.
/// Excel serial: days since 1899-12-30 (accounting for the Lotus 1-2-3 leap year bug).
fn date_to_excel_serial(date: chrono::NaiveDate) -> f64 {
    let base = chrono::NaiveDate::from_ymd_opt(1899, 12, 30).unwrap();
    (date - base).num_days() as f64
}

/// Format a number for XLSX: avoid scientific notation, reasonable precision.
fn format_number(n: f64) -> String {
    if n == n.floor() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        // Up to 10 decimal places, trim trailing zeros
        let s = format!("{:.10}", n);
        let s = s.trim_end_matches('0');
        let s = s.trim_end_matches('.');
        s.to_string()
    }
}

/// Escape text for XML content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn map_zip(e: zip::result::ZipError) -> OfferError {
    OfferError::Template(format!("ZIP error: {e}"))
}

fn map_io(e: std::io::Error) -> OfferError {
    OfferError::Template(format!("IO error: {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("Müller & Söhne"), "Müller &amp; Söhne");
        assert_eq!(xml_escape("a < b > c"), "a &lt; b &gt; c");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(30.0), "30");
        assert_eq!(format_number(51.29), "51.29");
        assert_eq!(format_number(2.1), "2.1");
    }

    #[test]
    fn test_extract_row_number() {
        assert_eq!(extract_row_number("A8"), 8);
        assert_eq!(extract_row_number("G14"), 14);
        assert_eq!(extract_row_number("J50"), 50);
        assert_eq!(extract_row_number("AB123"), 123);
    }

    #[test]
    fn test_hide_row_no_hidden_attr() {
        let xml = r#"<row r="31" spans="1:14" ht="12.75">"#;
        let result = hide_row(xml, 31);
        assert!(result.contains(r#"hidden="true""#));
    }

    #[test]
    fn test_hide_row_hidden_false() {
        let xml = r#"<row r="31" spans="1:14" hidden="false" ht="12.75">"#;
        let result = hide_row(xml, 31);
        assert!(result.contains(r#"hidden="true""#));
        assert!(!result.contains(r#"hidden="false""#));
    }

    #[test]
    fn test_set_cell_value_text() {
        let xml = r#"<row r="8"><c r="A8" s="4" t="s"><v>2</v></c></row>"#;
        let result = set_cell_value(xml, "A8", &CellValue::Text("Frau".into()));
        assert!(result.contains(r#"t="inlineStr">"#));
        assert!(result.contains("<is><t>Frau</t></is>"));
        assert!(!result.contains(r#"t="s""#));
    }

    #[test]
    fn test_set_cell_value_number() {
        let xml = r#"<row r="31"><c r="E31" s="34"><v>0</v></c></row>"#;
        let result = set_cell_value(xml, "E31", &CellValue::Number(1.0));
        assert!(result.contains("<v>1</v>"));
    }

    #[test]
    fn test_fix_print_area() {
        let xml = r#"<definedName name="_xlnm.Print_Area">Tabelle1!$A$1:$H$120,Tabelle1!$I$1:$P$43</definedName>"#;
        let result = fix_print_area(xml);
        assert_eq!(
            result,
            r#"<definedName name="_xlnm.Print_Area">Tabelle1!$A$1:$H$120</definedName>"#
        );
    }
}
