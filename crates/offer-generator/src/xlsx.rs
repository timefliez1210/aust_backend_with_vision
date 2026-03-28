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

/// All data needed to fill the XLSX offer template for one moving job.
///
/// **Caller**: `crates/api/src/routes/offers.rs` assembles this from the
///             database quote/offer record and passes it to `generate_offer_xlsx`.
/// **Why**: Acts as a single, serialisable transfer object between the HTTP
/// route and the XLSX generator so neither side leaks domain types into the
/// other's module.
///
/// Most string fields are pre-formatted for German display (e.g. `moving_date`
/// is already `"DD.MM.YYYY"`, `origin_floor_info` is already `"3. Stock"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferData {
    /// Offer reference number printed on the document title line.
    pub offer_number: String,
    /// Document creation date (written to cell G16 as an Excel serial number).
    pub date: chrono::NaiveDate,
    /// Optional validity expiry date (not currently written to the template).
    pub valid_until: Option<chrono::NaiveDate>,
    /// Customer salutation, e.g. `"Herrn"` or `"Frau"` (cell A8).
    pub customer_salutation: String,
    /// Full customer name (cell A9).
    pub customer_name: String,
    /// Customer street + house number (cell A10).
    pub customer_street: String,
    /// Customer postal code + city (cell A11).
    pub customer_city: String,
    /// Customer phone number (cell B18).
    pub customer_phone: String,
    /// Customer e-mail address (cell F18, same row as phone — rendered as plain text).
    pub customer_email: String,
    /// Opening salutation line, e.g. `"Sehr geehrter Herr Müller,"` (cell A20).
    pub greeting: String,
    /// Moving date as a pre-formatted German string, e.g. `"15.04.2026"` (cell B17).
    pub moving_date: String,
    /// Origin street + house number (cell A26).
    pub origin_street: String,
    /// Origin postal code + city (cell A27).
    pub origin_city: String,
    /// Origin floor description, e.g. `"3. Stock"` or `"EG"` (cell A28).
    pub origin_floor_info: String,
    /// Destination street + house number (cell F26).
    pub dest_street: String,
    /// Destination postal code + city (cell F27).
    pub dest_city: String,
    /// Destination floor description (cell F28).
    pub dest_floor_info: String,
    /// Estimated move volume in cubic metres (used in the "Umzugspauschale" label at A29).
    pub volume_m3: f64,
    /// Number of workers — written to cell J50 so the labor formula `G38 = E38*F38*J50` works.
    pub persons: u32,
    /// Estimated job duration in hours (used in the labor line item's quantity column E).
    pub estimated_hours: f64,
    /// Hourly rate per worker in euros (used in the labor line item's unit-price column F).
    pub rate_per_person_hour: f64,
    /// Line items written sequentially into rows 31-42 of the template.
    /// Maximum 12 items (slots 31-42). Extra items are silently truncated.
    pub line_items: Vec<OfferLineItem>,
    /// Optional detected/parsed inventory items added to the second sheet.
    /// If empty, no second sheet is created.
    pub detected_items: Vec<DetectedItemRow>,
}

/// A single line item written into one row of the XLSX offer template (rows 31-42).
///
/// **Caller**: `crates/api/src/routes/offers.rs` (`build_line_items` function)
/// **Why**: The offer template has a fixed set of rows for services. Each
/// `OfferLineItem` maps one-to-one to a template row and carries all the data
/// the XLSX generator needs to fill columns A-G correctly.
///
/// The generator writes items sequentially starting from row 31. Unused rows
/// are hidden so the PDF appears clean.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OfferLineItem {
    /// Service description written to column A (merged with B), e.g. `"Halteverbotszone"`.
    pub description: String,
    /// Quantity written to column E, e.g. number of parking ban zones or hours worked.
    pub quantity: f64,
    /// Unit price in euros written to column F, e.g. `100.0` for €100/zone.
    pub unit_price: f64,
    /// When `true`, column F is styled as an hourly rate (€/Stunde) and the
    /// G-column formula multiplies by `J50` (number of workers):
    /// `G = E × F × J50`.
    #[serde(default)]
    pub is_labor: bool,
    /// Optional remark written to column C (Bemerkung), e.g. a note about the service.
    #[serde(default)]
    pub remark: Option<String>,
    /// When set, columns E and F are left blank and G is written as this flat
    /// euro value directly (no formula). Used for Fahrkostenpauschale where
    /// the total is computed externally from the ORS route distance.
    #[serde(default)]
    pub flat_total: Option<f64>,
}

/// A single detected or inventory item written to the second XLSX sheet
/// ("Erfasste Gegenstände").
///
/// **Caller**: `crates/api/src/routes/offers.rs` and `crates/volume-estimator`
/// **Why**: When a customer submits photos or an inventory form, the vision
/// pipeline returns a list of recognised furniture items. This struct carries
/// one item through to the XLSX so Alex can review exactly what was detected.
///
/// Only `name` and `volume_m3` are strictly required. The remaining fields are
/// optional metadata surfaced from the ML pipeline for debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedItemRow {
    /// Item name as detected, may include a quantity prefix like `"2x Sofa"`.
    pub name: String,
    /// Volume contribution of this item in cubic metres.
    pub volume_m3: f64,
    /// Human-readable dimension string, e.g. `"2.00×0.90×0.80 m"` (optional).
    pub dimensions: Option<String>,
    /// Detection confidence score in `[0.0, 1.0]` from the ML model.
    pub confidence: f64,
    /// German display name from the RE catalogue lookup, if matched.
    #[serde(default)]
    pub german_name: Option<String>,
    /// RE (Raumeinheit) catalogue value — 1 RE = 0.1 m³.
    #[serde(default)]
    pub re_value: Option<f64>,
    /// Which method produced the volume: `"re_lookup"`, `"geometric"`, etc.
    #[serde(default)]
    pub volume_source: Option<String>,
    /// S3 key of the cropped detection image, for debugging in the admin UI.
    #[serde(default)]
    pub crop_s3_key: Option<String>,
    /// Bounding box `[x1, y1, x2, y2]` in normalised image coordinates.
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    /// Index of the source image this detection came from (0-based).
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    /// Pre-signed S3 URLs of the source images this item was detected in.
    #[serde(default)]
    pub source_image_urls: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Cell value types for modifications
// ---------------------------------------------------------------------------

/// Internal representation of the value to write into a single XLSX cell.
///
/// **Why**: The XLSX XML format represents text, numbers, and formulas
/// differently (`t="inlineStr"`, `t="n"`, `<f>` element). This enum lets the
/// XML-building code branch cleanly without separate functions for each type.
///
/// The `Styled*` variants carry a hard-coded style index string that overrides
/// whatever style the template cell already has. The un-styled variants
/// (`Text`, `Number`) preserve the original template style.
enum CellValue {
    /// Plain UTF-8 text; preserves the existing template cell style.
    Text(String),
    /// Text with an explicit style index string (overrides the template cell's style).
    /// The `&'static str` is a decimal style index from `xl/styles.xml`, e.g. `"58"`.
    StyledText(String, &'static str),
    /// Numeric value; preserves the existing template cell style.
    Number(f64),
    /// Numeric value with an explicit style index (used when inserting into a new cell
    /// that has no pre-existing style in the template).
    StyledNumber(f64, &'static str),
    /// Excel formula string with an explicit style index.
    /// The cached `<v>` element is omitted so LibreOffice must recalculate on open.
    StyledFormula(String, &'static str),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a complete XLSX offer document from the embedded template and offer data.
///
/// **Caller**: `crates/api/src/routes/offers.rs` — called from both the initial
///             generation path and the Telegram edit/regenerate path.
/// **Why**: The XLSX template already has company branding, borders, merged cells,
/// print settings, and formulas set up. This function surgically replaces only the
/// dynamic cell values, hiding/showing rows as needed, without touching any of the
/// template's formatting or structure.
///
/// High-level steps:
/// 1. Open the embedded template ZIP.
/// 2. Build all cell modifications, hidden-row lists, and unhidden-row lists from `data`.
/// 3. Apply modifications to `sheet1.xml` (cell values, row visibility, formula caches).
/// 4. Strip hyperlinks from `sheet1.xml` and its `.rels` file (email as plain text).
/// 5. Patch `workbook.xml`: fix print area to A:H, force recalculation, add items sheet ref.
/// 6. Patch `[Content_Types].xml` and `workbook.xml.rels` if an items sheet is needed.
/// 7. Fix date format in `styles.xml` from `m/d/yyyy` to `dd.mm.yyyy`.
/// 8. Re-assemble all XML files back into a new ZIP.
///
/// # Parameters
/// - `data` — fully populated `OfferData` struct
///
/// # Returns
/// Raw bytes of a valid `.xlsx` file ready to pass to `convert_xlsx_to_pdf`.
///
/// # Errors
/// - `OfferError::Template` if the embedded template ZIP is corrupt
/// - `OfferError::Template` if any internal XML file is not valid UTF-8
/// - `OfferError::Template` if ZIP reassembly fails
pub fn generate_offer_xlsx(data: &OfferData) -> Result<Vec<u8>, OfferError> {
    let mut template_zip = ZipArchive::new(Cursor::new(TEMPLATE_BYTES))
        .map_err(|e| OfferError::Template(format!("Failed to read template ZIP: {e}")))?;

    // Build modifications
    let (cell_mods, hidden_rows, unhidden_rows) = build_cell_modifications(data);

    // Read and modify sheet1.xml
    let sheet1_xml = read_zip_entry(&mut template_zip, "xl/worksheets/sheet1.xml")?;
    let sheet1_str = String::from_utf8(sheet1_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml is not valid UTF-8: {e}")))?;
    let mut modified_sheet1 =
        apply_modifications(&sheet1_str, &cell_mods, &hidden_rows, &unhidden_rows);

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

/// Build the complete list of cell value changes, rows to hide, and rows to show.
///
/// **Why**: Separates the "what to change" logic from the "how to change XML" logic.
/// All domain knowledge about which cell holds which offer field lives here, not in
/// the XML manipulation functions.
///
/// The function hides ALL template line-item rows (31-42) first, then writes items
/// sequentially starting at row 31, un-hiding only the rows actually used. This
/// ensures unused rows (and their preset values) never appear in the PDF.
///
/// # Parameters
/// - `data` — the fully populated `OfferData` to read field values from
///
/// # Returns
/// A tuple of:
/// - `Vec<(String, CellValue)>` — `(cell_ref, value)` pairs to apply to `sheet1.xml`
/// - `Vec<u32>` — row numbers to hide (all unused line-item rows)
/// - `Vec<u32>` — row numbers to un-hide (only the rows that contain a line item)
fn build_cell_modifications(
    data: &OfferData,
) -> (Vec<(String, CellValue)>, Vec<u32>, Vec<u32>) {
    let mut mods = Vec::new();

    // Customer address block
    mods.push(("A8".into(), CellValue::Text(data.customer_salutation.clone())));
    mods.push(("A9".into(), CellValue::Text(data.customer_name.clone())));
    mods.push(("A10".into(), CellValue::Text(data.customer_street.clone())));
    mods.push(("A11".into(), CellValue::Text(data.customer_city.clone())));

    // The drawing text box (company contact block) anchors to rows 9–14 in the template,
    // overlapping cell G14. Writing the date to G15 (just below) keeps it visible and
    // prevents it from being obscured by the text box border.
    mods.push(("G14".into(), CellValue::Text(String::new()))); // clear the TODAY() formula
    mods.push(("G15".into(), CellValue::Text(data.date.format("%d.%m.%Y").to_string())));
    // G16 is the title row — do not write the date there.

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
    // E-Mail on the same row as the phone number (F18), matching the template label at E18.
    mods.push(("F18".into(), CellValue::StyledText(data.customer_email.clone(), "0")));

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

    // --- Line items: dynamic row assignment with alternating styles ---
    //
    // Template column layout for rows 31-42:
    //   A-B (merged): Beschreibung (main description)
    //   C:            Bemerkung (remark)
    //   D:            (mostly empty)
    //   E:            Menge (quantity)
    //   F:            Einzelpreis (unit price)
    //   G:            Gesamt Netto (formula)
    //
    // Style pairs indexed by [color]: 0 = white (fill 4), 1 = blue (fill 3)
    const STYLES_AB: [&str; 2] = ["58", "53"]; // columns A, B
    const STYLES_C: [&str; 2] = ["59", "54"];  // column C
    const STYLES_DE: [&str; 2] = ["60", "55"]; // columns D, E
    const STYLES_F: [&str; 2] = ["61", "56"];          // column F (€)
    const STYLES_F_LABOR: [&str; 2] = ["86", "85"];   // column F labor (€/Stunde)
    const STYLES_G: [&str; 2] = ["84", "83"];          // column G (right-aligned €)

    // 1. Hide ALL template rows 31-42 and clear their content
    let mut hidden_rows: Vec<u32> = (31..=42).collect();
    let mut unhidden_rows: Vec<u32> = Vec::new();

    for row in 31..=42u32 {
        mods.push((format!("E{row}"), CellValue::Number(0.0)));
        mods.push((format!("F{row}"), CellValue::Number(0.0)));
    }

    // 2. Write items sequentially starting at row 31
    let max_items = 12.min(data.line_items.len()); // template has 12 slots (31-42)
    for (i, item) in data.line_items.iter().take(max_items).enumerate() {
        let row = 31 + i as u32;
        let color = 1 - i % 2; // 1 = blue (first row), 0 = white

        // Un-hide this row
        unhidden_rows.push(row);

        // Write description to column A (merged with B = "Beschreibung")
        mods.push((
            format!("A{row}"),
            CellValue::StyledText(item.description.clone(), STYLES_AB[color]),
        ));
        // Style column B (merge partner, content ignored but style visible)
        mods.push((
            format!("B{row}"),
            CellValue::StyledText(String::new(), STYLES_AB[color]),
        ));
        // Column C: Bemerkung (remark)
        let remark = item.remark.as_deref().unwrap_or("");
        mods.push((
            format!("C{row}"),
            CellValue::StyledText(remark.to_string(), STYLES_C[color]),
        ));
        // Clear column D (mostly empty, but some rows have preset text)
        mods.push((
            format!("D{row}"),
            CellValue::StyledText(String::new(), STYLES_DE[color]),
        ));
        if let Some(ft) = item.flat_total {
            // Flat-total item (e.g. Fahrkostenpauschale): E and F blank, G = flat value
            mods.push((
                format!("E{row}"),
                CellValue::StyledText(String::new(), STYLES_DE[color]),
            ));
            mods.push((
                format!("F{row}"),
                CellValue::StyledText(String::new(), STYLES_F[color]),
            ));
            mods.push((format!("G{row}"), CellValue::StyledNumber(ft, STYLES_G[color])));
        } else {
            // Write quantity
            mods.push((
                format!("E{row}"),
                CellValue::StyledNumber(item.quantity, STYLES_DE[color]),
            ));
            // Write unit price (€ format, labor uses €/Stunde)
            let f_style = if item.is_labor { STYLES_F_LABOR[color] } else { STYLES_F[color] };
            mods.push((
                format!("F{row}"),
                CellValue::StyledNumber(item.unit_price, f_style),
            ));

            // Write formula to G with right-aligned € style
            if item.is_labor {
                mods.push(("J50".into(), CellValue::Number(data.persons as f64)));
                let formula = format!("IF(E{row}=\"\", 0, F{row}*E{row}*J50)");
                mods.push((format!("G{row}"), CellValue::StyledFormula(formula, STYLES_G[color])));
            } else {
                let formula = format!("IF(E{row}=\"\", 0, F{row}*E{row})");
                mods.push((format!("G{row}"), CellValue::StyledFormula(formula, STYLES_G[color])));
            }
        }
    }

    // Remove unhidden rows from hidden list
    hidden_rows.retain(|r| !unhidden_rows.contains(r));

    // Rewrite G44 formula: SUM instead of individual row references (keep € right-aligned style)
    mods.push(("G44".into(), CellValue::StyledFormula("SUM(G31:G42)".into(), "72")));

    (mods, hidden_rows, unhidden_rows)
}

// ---------------------------------------------------------------------------
// XML modification engine
// ---------------------------------------------------------------------------

/// Apply all cell modifications, row-hide operations, and row-unhide operations to
/// a `sheet1.xml` string, then strip stale formula cached values.
///
/// **Why**: The XLSX XML manipulation approach avoids a full parse-and-serialize
/// round-trip (which would lose style indices, merge-cell info, etc.). Instead,
/// targeted string surgery is used — the template XML structure is stable and
/// well-known, so positional searches are reliable.
///
/// # Parameters
/// - `xml` — raw content of `xl/worksheets/sheet1.xml` from the template
/// - `cell_mods` — `(cell_ref, value)` pairs; applied in order
/// - `hidden_rows` — row numbers to add `hidden="true"` to
/// - `unhidden_rows` — row numbers to set `hidden="false"` on (overrides any
///   pre-existing hidden flag from the template)
///
/// # Returns
/// Modified XML string ready to be written back into the output ZIP.
fn apply_modifications(
    xml: &str,
    cell_mods: &[(String, CellValue)],
    hidden_rows: &[u32],
    unhidden_rows: &[u32],
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

    // 3. Un-hide rows (ensure visible rows are not hidden)
    for &row_num in unhidden_rows {
        result = unhide_row(&result, row_num);
    }

    // 4. Strip cached values from formula cells so LibreOffice must recalculate.
    //    Template has stale <v> values that won't match our new inputs.
    result = strip_formula_cached_values(&result);

    result
}

/// Replace the value of a single cell in `sheet1.xml`.
///
/// **Why**: Handles the three real-world cases that arise from the template:
/// 1. The `<c>` element already exists with child elements — replace the whole element.
/// 2. The `<c>` element is self-closing `<c ... />` — replace with a full element.
/// 3. The cell does not exist at all — insert it into the correct row (or create the row).
///
/// The function preserves the original cell's `s="N"` style attribute unless the
/// `CellValue` variant is a `Styled*` type, in which case the provided style wins.
///
/// # Parameters
/// - `xml` — the current `sheet1.xml` content
/// - `cell_ref` — Excel address string, e.g. `"A8"`, `"G44"`, `"J50"`
/// - `value` — the `CellValue` variant determining type and content of the new cell
///
/// # Returns
/// A new `String` with the cell replaced. Returns the original string unchanged if
/// the cell cannot be located or its boundaries cannot be parsed.
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

/// Find the byte offset just past the end of a `<c ...>...</c>` or `<c .../>` element.
///
/// **Why**: XLSX cell elements can be self-closing (`<c r="A1"/>`) or have
/// nested children (`<c r="A1"><v>42</v></c>`). A depth-counting byte scan handles
/// both cases without a full XML parser.
///
/// # Parameters
/// - `fragment` — a string slice starting at the opening `<c` of the cell element
///
/// # Returns
/// `Some(offset)` — byte offset from the start of `fragment` to one byte past the
/// closing `/>` or `</c>`, ready to use as a slice upper bound.
/// `None` if the element boundary cannot be found (malformed XML).
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

/// Build a complete `<c>` XML element string for the given cell reference and value.
///
/// **Why**: Centralises the XLSX XML cell format so all cell-writing paths
/// produce consistent, valid XML. Each `CellValue` variant maps to a specific
/// OOXML cell type:
/// - `Text` / `StyledText` → `t="inlineStr"` with `<is><t>…</t></is>`
/// - `Number` / `StyledNumber` → `t="n"` with `<v>…</v>`
/// - `StyledFormula` → `t="n"` with `<f>…</f>` (no cached `<v>`)
///
/// # Parameters
/// - `cell_ref` — Excel address string, e.g. `"A8"`
/// - `style` — optional style index from the original cell; ignored for `Styled*` variants
/// - `value` — the value type and content to encode
///
/// # Returns
/// A complete, self-contained `<c>…</c>` XML string.
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
        CellValue::StyledText(text, forced_style) => {
            let escaped = xml_escape(text);
            format!(
                r#"<c r="{}" s="{}" t="inlineStr"><is><t>{}</t></is></c>"#,
                cell_ref, forced_style, escaped
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
        CellValue::StyledFormula(formula, forced_style) => {
            let escaped = xml_escape(formula);
            format!(
                r#"<c r="{}" s="{}" t="n"><f>{}</f></c>"#,
                cell_ref, forced_style, escaped
            )
        }
    }
}

/// Insert a new `<c>` element into the sheet XML for a cell that does not yet exist.
///
/// **Why**: Not every cell in the template has an explicit `<c>` element — Excel
/// omits cells that are empty. When the generator needs to write to such a cell
/// (e.g. J50 for the persons count), it must inject a new element into the
/// correct `<row>` block.
///
/// Falls back to inserting a new `<row>` containing the cell just before
/// `</sheetData>` if the row itself does not exist in the template.
///
/// # Parameters
/// - `xml` — current `sheet1.xml` content
/// - `cell_ref` — Excel address of the new cell, e.g. `"J50"`
/// - `value` — the value to write into the new cell
///
/// # Returns
/// Modified XML string with the new cell inserted. Returns the original string
/// unchanged if neither the row nor `<sheetData>` can be located.
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

/// Set `hidden="true"` on the `<row>` element for the given row number.
///
/// **Why**: Unused line-item rows in the template (31-42) are hidden so they
/// don't appear in the PDF. The function handles two sub-cases: the row already
/// has `hidden="false"` (replace it), or has no `hidden` attribute at all (insert it).
///
/// # Parameters
/// - `xml` — current `sheet1.xml` content
/// - `row_num` — 1-based XLSX row number to hide
///
/// # Returns
/// Modified XML string. Returns the original if the row is not found.
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

/// Set `hidden="false"` on the `<row>` element for the given row number.
///
/// **Why**: Template rows that were previously hidden (e.g. from a prior generation
/// cycle baked into the template) must be explicitly un-hidden when the generator
/// wants to use them. The function replaces `hidden="true"` with `hidden="false"`;
/// if no hidden attribute is present the row is already visible and no change is made.
///
/// # Parameters
/// - `xml` — current `sheet1.xml` content
/// - `row_num` — 1-based XLSX row number to make visible
///
/// # Returns
/// Modified XML string. Returns the original if the row is not found or already visible.
fn unhide_row(xml: &str, row_num: u32) -> String {
    let row_r = format!(r#"<row r="{}""#, row_num);
    if let Some(pos) = xml.find(&row_r) {
        let after = &xml[pos..];
        if let Some(gt_offset) = after.find('>') {
            let gt_pos = pos + gt_offset;
            let tag_content = &xml[pos..gt_pos];

            // Replace hidden="true" with hidden="false"
            if let Some(h_pos) = tag_content.find(r#"hidden="true""#) {
                let abs_h = pos + h_pos;
                let mut result = String::with_capacity(xml.len());
                result.push_str(&xml[..abs_h]);
                result.push_str(r#"hidden="false""#);
                result.push_str(&xml[abs_h + r#"hidden="true""#.len()..]);
                return result;
            }
        }
    }
    xml.to_string()
}

/// Remove stale `<v>…</v>` cached values from all formula cells in `sheet1.xml`.
///
/// **Why**: The XLSX template contains formula cells with cached `<v>` values
/// from the last time the template was saved in Excel. After the generator rewrites
/// E/F cells with new quantities and prices, those cached values no longer match.
/// LibreOffice will display the stale cache if it exists, so stripping them forces
/// a full recalculation on open. Combined with `fullCalcOnLoad="true"` in `workbook.xml`,
/// this guarantees the PDF reflects the actual formula results.
///
/// # Parameters
/// - `xml` — `sheet1.xml` content after all cell modifications have been applied
///
/// # Returns
/// XML string with `<v>…</v>` and `<v/>` elements removed from every cell that
/// also contains a `<f>…</f>` formula element.
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

/// Remove the `<v>…</v>` or `<v/>` cached-value element from a single cell XML fragment.
///
/// **Why**: Called by `strip_formula_cached_values` for each formula cell. Handles
/// both the common `<v>number</v>` form and the rare empty `<v/>` variant.
///
/// # Parameters
/// - `cell` — the full `<c>…</c>` XML fragment for one cell
///
/// # Returns
/// The cell fragment with the `<v>` element removed. Returns the original string
/// unchanged if no `<v>` element is found.
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

/// Patch `workbook.xml`: fix the print area, force formula recalculation, and
/// optionally register the items sheet.
///
/// **Why**: The template's `workbook.xml` has a dual print area
/// (`A:H` + `I:P`) that would include the internal calculation columns in the PDF.
/// This function narrows it to `A:H` only. It also sets `fullCalcOnLoad="true"`
/// so LibreOffice recalculates all formulas when it opens the file.
///
/// # Parameters
/// - `xml` — raw content of `xl/workbook.xml`
/// - `add_items_sheet` — when `true`, inject a `<sheet>` element for sheet2
///
/// # Returns
/// Modified `workbook.xml` content string.
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

/// Add `fullCalcOnLoad="true"` to the `<calcPr>` element in `workbook.xml`.
///
/// **Why**: Without this flag, LibreOffice trusts the stale cached `<v>` values
/// in formula cells. Even after `strip_formula_cached_values` removes those caches,
/// `fullCalcOnLoad` is needed as a belt-and-suspenders measure to guarantee that
/// the SUM in G44 and all line-item totals are recalculated before PDF rendering.
///
/// # Parameters
/// - `xml` — raw `workbook.xml` content
///
/// # Returns
/// Modified string with `fullCalcOnLoad="true"` injected into `<calcPr>`.
/// Returns the original if `<calcPr>` is not found.
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

/// Replace the dual-range `_xlnm.Print_Area` defined name with `Tabelle1!$A$1:$H$120`.
///
/// **Why**: The template was saved with a print area that includes two ranges:
/// `Tabelle1!$A$1:$H$120,Tabelle1!$I$1:$P$43`. Columns I-P hold internal
/// calculation helper values (e.g. J50 for worker count) that must not appear
/// in the customer-facing PDF. Replacing the defined name removes them from
/// the print area before LibreOffice converts the file.
///
/// # Parameters
/// - `xml` — raw `workbook.xml` content
///
/// # Returns
/// Modified string with the print area limited to columns A-H.
/// Returns the original if `_xlnm.Print_Area` is not found.
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

/// Inject a `<sheet>` element for "Erfasste Gegenstände" into `workbook.xml`.
///
/// **Why**: The items sheet (sheet2.xml) is dynamically created when the offer has
/// detected inventory items. The workbook must reference it so Excel/LibreOffice
/// recognises the sheet exists. The sheet is assigned relationship ID `rId5`
/// (chosen to avoid collisions with the template's existing rId1-rId4).
///
/// # Parameters
/// - `xml` — raw `workbook.xml` content
///
/// # Returns
/// Modified string with the `<sheet>` element inserted just before `</sheets>`.
/// Returns the original if `</sheets>` is not found.
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

/// Build the complete XML for the "Erfasste Gegenstände" (detected items) second sheet.
///
/// **Why**: When a customer submits photos or an inventory form, the detected items
/// are shown on a separate sheet so Alex can review them alongside the offer. The
/// sheet lists each item's sequential number, name (with quantity prefix stripped),
/// quantity, and volume in m³, with a bold orange total row at the bottom.
///
/// Style indices reference the shared `xl/styles.xml` from the template ZIP:
/// - `79` = orange header background, bold white, left-aligned
/// - `80` = orange header background, bold white, centre-aligned
/// - `81` = white-fill data row, centre-aligned
/// - `82` = blue-fill (odd) data row, centre-aligned
///
/// # Parameters
/// - `items` — slice of detected/inventory items; must be non-empty (caller checks)
///
/// # Returns
/// A complete `xl/worksheets/sheet2.xml` content string ready to write into the output ZIP.
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

    // Header row (orange background, bold white text)
    xml.push_str(r#"<row r="1" ht="18">"#);
    for (col, header, s) in [("A", "Nr.", "80"), ("B", "Gegenstand", "79"), ("C", "Anzahl", "80"), ("D", "Volumen (m³)", "80")] {
        xml.push_str(&format!(
            r#"<c r="{}1" s="{}" t="inlineStr"><is><t>{}</t></is></c>"#,
            col, s, xml_escape(header)
        ));
    }
    xml.push_str(r#"</row>"#);

    // Data rows
    let mut total_volume = 0.0;
    let mut total_quantity: u32 = 0;
    for (i, item) in items.iter().enumerate() {
        let row = i + 2;
        let is_odd = i % 2 == 1;
        let s_center = if is_odd { "82" } else { "81" };

        // Parse quantity prefix from name (e.g. "2x Sideboard groß" → 2, strip prefix)
        let (qty, display_name): (u32, &str) = item
            .name
            .find('x')
            .and_then(|pos| {
                let prefix = item.name[..pos].trim();
                let q: u32 = prefix.parse().ok()?;
                Some((q, item.name[pos + 1..].trim()))
            })
            .unwrap_or((1, &item.name));
        total_quantity += qty;

        xml.push_str(&format!(r#"<row r="{}">"#, row));

        xml.push_str(&format!(
            r#"<c r="A{}" s="{}" t="n"><v>{}</v></c>"#,
            row, s_center, i + 1
        ));
        xml.push_str(&format!(
            r#"<c r="B{}" s="{}" t="inlineStr"><is><t>{}</t></is></c>"#,
            row, s_center, xml_escape(display_name)
        ));

        xml.push_str(&format!(
            r#"<c r="C{}" s="{}" t="n"><v>{}</v></c>"#,
            row, s_center, qty
        ));

        xml.push_str(&format!(
            r#"<c r="D{}" s="{}" t="inlineStr"><is><t>{:.2} m³</t></is></c>"#,
            row, s_center, item.volume_m3
        ));

        xml.push_str(r#"</row>"#);
        total_volume += item.volume_m3;
    }

    // Total row (orange background, bold white text)
    let total_row = items.len() + 2;
    xml.push_str(&format!(r#"<row r="{}" ht="18">"#, total_row));
    xml.push_str(&format!(
        r#"<c r="A{}" s="80" t="inlineStr"><is><t></t></is></c>"#,
        total_row
    ));
    xml.push_str(&format!(
        r#"<c r="B{}" s="79" t="inlineStr"><is><t>Gesamt</t></is></c>"#,
        total_row
    ));
    xml.push_str(&format!(
        r#"<c r="C{}" s="80" t="n"><v>{}</v></c>"#,
        total_row, total_quantity
    ));
    xml.push_str(&format!(
        r#"<c r="D{}" s="80" t="inlineStr"><is><t>{:.2} m³</t></is></c>"#,
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

/// Register `xl/worksheets/sheet2.xml` in `[Content_Types].xml`.
///
/// **Why**: Every part of an OOXML package must have a corresponding `<Override>`
/// entry in `[Content_Types].xml` or the file is invalid. Without this entry,
/// Excel and LibreOffice silently ignore sheet2.xml.
///
/// # Parameters
/// - `xml` — raw `[Content_Types].xml` content
///
/// # Returns
/// Modified string with the sheet2 override entry inserted before `</Types>`.
/// Returns the original if `</Types>` is not found.
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

/// Add a `<Relationship>` entry for sheet2 into `xl/_rels/workbook.xml.rels`.
///
/// **Why**: The workbook's relationship file maps logical relationship IDs (like `rId5`)
/// to physical file paths (like `worksheets/sheet2.xml`). Without this entry,
/// the `<sheet r:id="rId5">` element added to `workbook.xml` has no target and
/// the XLSX package is invalid.
///
/// # Parameters
/// - `xml` — raw `xl/_rels/workbook.xml.rels` content
///
/// # Returns
/// Modified string with the sheet2 relationship inserted before `</Relationships>`.
/// Returns the original if `</Relationships>` is not found.
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

/// Remove the `<hyperlinks>…</hyperlinks>` section from `sheet1.xml`.
///
/// **Why**: The template was saved with email addresses formatted as clickable
/// hyperlinks. When LibreOffice converts the file to PDF it renders those as
/// blue underlined links. The offer document should display the customer's email
/// as plain text matching the rest of the address block. Removing the
/// `<hyperlinks>` block demotes those cells to plain inline strings.
///
/// The companion function `strip_hyperlink_rels` removes the corresponding
/// relationship entries from `sheet1.xml.rels`.
///
/// # Parameters
/// - `xml` — raw `sheet1.xml` content
///
/// # Returns
/// Modified string with the entire `<hyperlinks>…</hyperlinks>` block removed.
/// Returns the original if the block is not found.
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

/// Remove all hyperlink `<Relationship>` entries from `xl/worksheets/_rels/sheet1.xml.rels`.
///
/// **Why**: Each hyperlink in the sheet has a corresponding relationship entry of
/// type `…/hyperlink`. After removing the `<hyperlinks>` block from `sheet1.xml`,
/// these relationship entries are orphaned and can cause validation warnings. The
/// drawing relationship (for the company logo image) is preserved.
///
/// # Parameters
/// - `xml` — raw `xl/worksheets/_rels/sheet1.xml.rels` content
///
/// # Returns
/// Modified string with all `<Relationship>` elements containing `"hyperlink"` removed.
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

/// Reassemble the final XLSX ZIP from the template and all modified XML files.
///
/// **Why**: XLSX is a ZIP of XML files. The generator modifies only six of those
/// files; all others (drawings, media, shared strings, theme, etc.) are copied
/// bit-for-bit from the template. This function iterates over every entry in the
/// template ZIP, replaces modified entries by name, and optionally appends
/// `sheet2.xml` for the detected items.
///
/// # Parameters
/// - `template_zip` — the already-opened template `ZipArchive`
/// - `sheet1_xml` — modified `xl/worksheets/sheet1.xml` content
/// - `workbook_xml` — modified `xl/workbook.xml` content
/// - `content_types_xml` — modified `[Content_Types].xml` content
/// - `rels_xml` — modified `xl/_rels/workbook.xml.rels` content
/// - `sheet1_rels_xml` — modified `xl/worksheets/_rels/sheet1.xml.rels` content
/// - `styles_xml` — modified `xl/styles.xml` content (German date format)
/// - `items_sheet_xml` — optional `xl/worksheets/sheet2.xml` content; appended if `Some`
///
/// # Returns
/// Raw bytes of the assembled `.xlsx` file.
///
/// # Errors
/// - `OfferError::Template` if any template ZIP entry cannot be read
/// - `OfferError::Template` if writing to the output ZIP fails
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

/// Read a named entry from a `ZipArchive` into a `Vec<u8>`.
///
/// **Why**: Every XML file in the template ZIP must be extracted before it can be
/// modified. This helper centralises the error mapping from `zip::ZipError` and
/// `std::io::Error` to `OfferError::Template`.
///
/// # Parameters
/// - `archive` — the open `ZipArchive` wrapping the template bytes
/// - `name` — the ZIP entry path, e.g. `"xl/worksheets/sheet1.xml"`
///
/// # Returns
/// Raw byte content of the ZIP entry.
///
/// # Errors
/// - `OfferError::Template` if the entry does not exist in the archive
/// - `OfferError::Template` if reading the entry fails
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

/// Extract the value of a named XML attribute from a tag fragment string.
///
/// **Why**: Used by `set_cell_value` to preserve the existing `s="N"` style index
/// from the original template cell when building the replacement `<c>` element.
///
/// Assumes the value is enclosed in double-quotes (`attr="value"`). Single-quote
/// form is not handled since OOXML always uses double quotes.
///
/// # Parameters
/// - `tag` — a substring of XML containing the attribute, e.g. `r#"<c r="A8" s="4""#`
/// - `attr_name` — the attribute name to look up, e.g. `"s"`
///
/// # Returns
/// `Some(value)` with the attribute's string value, or `None` if not found.
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

/// Extract the numeric row index from an Excel cell reference string.
///
/// **Why**: Used by `insert_cell` to find the correct `<row r="N">` element when
/// a cell needs to be added to the sheet XML.
///
/// # Parameters
/// - `cell_ref` — an Excel cell address, e.g. `"A8"`, `"G44"`, `"AB123"`
///
/// # Returns
/// The `u32` row number. Returns `1` if no digit sequence is found.
///
/// # Examples
/// ```
/// // "A8" → 8, "G44" → 44, "J50" → 50, "AB123" → 123
/// ```
fn extract_row_number(cell_ref: &str) -> u32 {
    let num_start = cell_ref.find(|c: char| c.is_ascii_digit()).unwrap_or(0);
    cell_ref[num_start..].parse().unwrap_or(1)
}


/// Format an `f64` as a decimal string suitable for embedding in XLSX XML.
///
/// **Why**: Rust's default `Display` for `f64` can emit scientific notation
/// (`1e15`) or excessive decimal places, both of which are invalid in XLSX `<v>`
/// elements or produce rendering artefacts in Excel. This function produces the
/// shortest clean decimal representation.
///
/// Integer-valued floats are formatted without a decimal point (`30.0` → `"30"`).
/// Non-integer values are formatted with up to 10 decimal places and then have
/// trailing zeros and the trailing decimal point trimmed.
///
/// # Parameters
/// - `n` — the number to format
///
/// # Returns
/// Decimal string representation, e.g. `"30"`, `"51.29"`, `"2.1"`.
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

/// Escape a string for safe embedding in XML text content or attribute values.
///
/// **Why**: Customer data (names, addresses) can contain `&`, `<`, `>`, `"`, and `'`
/// characters that are XML metacharacters. Unescaped, they would produce malformed
/// XLSX XML and potentially a corrupt file.
///
/// # Parameters
/// - `s` — the raw string to escape
///
/// # Returns
/// A new `String` with the five XML special characters replaced by their entity
/// references: `&` → `&amp;`, `<` → `&lt;`, `>` → `&gt;`, `"` → `&quot;`,
/// `'` → `&apos;`.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Map a `zip::result::ZipError` to `OfferError::Template`.
///
/// Used as a closure in `.map_err(map_zip)` calls when writing ZIP entries
/// so the error type is consistent throughout `assemble_xlsx`.
fn map_zip(e: zip::result::ZipError) -> OfferError {
    OfferError::Template(format!("ZIP error: {e}"))
}

/// Map a `std::io::Error` to `OfferError::Template`.
///
/// Used as a closure in `.map_err(map_io)` calls when writing raw bytes
/// into ZIP entries inside `assemble_xlsx`.
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

    // --- End-to-end XLSX generation tests ---

    fn read_xlsx_sheet1(bytes: &[u8]) -> String {
        use std::io::Read as _;
        let cursor = std::io::Cursor::new(bytes);
        let mut archive = zip::ZipArchive::new(cursor).expect("valid zip");
        let mut file = archive
            .by_name("xl/worksheets/sheet1.xml")
            .expect("sheet1 exists");
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .expect("read sheet1");
        contents
    }

    fn minimal_offer_data() -> OfferData {
        OfferData {
            offer_number: "TEST-001".to_string(),
            date: chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            valid_until: None,
            customer_salutation: "Herrn".to_string(),
            customer_name: "Max Mustermann".to_string(),
            customer_street: "Musterstr. 1".to_string(),
            customer_city: "31135 Hildesheim".to_string(),
            customer_phone: "+491234567890".to_string(),
            customer_email: "max@example.com".to_string(),
            greeting: "Sehr geehrter Herr Mustermann,".to_string(),
            moving_date: "01.04.2026".to_string(),
            origin_street: "Auszugstr. 1".to_string(),
            origin_city: "31135 Hildesheim".to_string(),
            origin_floor_info: "3. Stock".to_string(),
            dest_street: "Zielstr. 5".to_string(),
            dest_city: "30159 Hannover".to_string(),
            dest_floor_info: "EG".to_string(),
            volume_m3: 20.0,
            persons: 3,
            estimated_hours: 4.0,
            rate_per_person_hour: 30.0,
            line_items: vec![],
            detected_items: vec![],
        }
    }

    #[test]
    fn generate_returns_valid_zip() {
        let data = minimal_offer_data();
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        assert!(bytes.len() > 0);
        // ZIP magic bytes: PK (0x50, 0x4B)
        assert_eq!(bytes[0], 0x50);
        assert_eq!(bytes[1], 0x4B);
    }

    #[test]
    fn j50_contains_persons_count() {
        let mut data = minimal_offer_data();
        data.persons = 4;
        // Need a labor line item to trigger J50 write
        data.line_items = vec![OfferLineItem {
            description: "4 Umzugshelfer".to_string(),
            quantity: 5.0,
            unit_price: 30.0,
            is_labor: true,
            flat_total: None,
            remark: None,
        }];
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let xml = read_xlsx_sheet1(&bytes);
        // J50 should be present with value 4
        assert!(xml.contains(r#"r="J50""#), "J50 cell should exist in XML");
        assert!(xml.contains("<v>4</v>"), "J50 should contain value 4");
    }

    #[test]
    fn flat_total_item_g_cell_has_value() {
        let mut data = minimal_offer_data();
        data.line_items = vec![OfferLineItem {
            description: "Fahrkostenpauschale".to_string(),
            quantity: 0.0,
            unit_price: 0.0,
            is_labor: false,
            flat_total: Some(75.0),
            remark: None,
        }];
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let xml = read_xlsx_sheet1(&bytes);
        // G31 should contain flat total 75
        assert!(xml.contains(r#"r="G31""#), "G31 cell should exist");
        // The flat_total is written as StyledNumber, so it should be <v>75</v>
        // Find the G31 cell and check it has the value
        let g31_pos = xml.find(r#"r="G31""#).unwrap();
        let g31_region = &xml[g31_pos..g31_pos + 200.min(xml.len() - g31_pos)];
        assert!(
            g31_region.contains("<v>75</v>"),
            "G31 should contain flat_total value 75, got: {}",
            g31_region
        );
        // E31 should be styled text (blank), not a number
        let e31_pos = xml.find(r#"r="E31""#).unwrap();
        let e31_region = &xml[e31_pos..e31_pos + 200.min(xml.len() - e31_pos)];
        assert!(
            e31_region.contains("t=\"inlineStr\""),
            "E31 should be inlineStr (blank) for flat_total item, got: {}",
            e31_region
        );
    }

    #[test]
    fn normal_item_e_f_cells_have_values() {
        let mut data = minimal_offer_data();
        data.line_items = vec![OfferLineItem {
            description: "Halteverbotszone".to_string(),
            quantity: 2.0,
            unit_price: 100.0,
            is_labor: false,
            flat_total: None,
            remark: None,
        }];
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let xml = read_xlsx_sheet1(&bytes);
        // E31 should have quantity 2
        let e31_pos = xml.find(r#"r="E31""#).unwrap();
        let e31_region = &xml[e31_pos..e31_pos + 200.min(xml.len() - e31_pos)];
        assert!(
            e31_region.contains("<v>2</v>"),
            "E31 should contain quantity 2, got: {}",
            e31_region
        );
        // F31 should have unit price 100
        let f31_pos = xml.find(r#"r="F31""#).unwrap();
        let f31_region = &xml[f31_pos..f31_pos + 200.min(xml.len() - f31_pos)];
        assert!(
            f31_region.contains("<v>100</v>"),
            "F31 should contain unit_price 100, got: {}",
            f31_region
        );
    }

    #[test]
    fn versicherung_item_has_zero_total() {
        let mut data = minimal_offer_data();
        data.line_items = vec![OfferLineItem {
            description: "Nürnbergerversicherung".to_string(),
            quantity: 0.0,
            unit_price: 0.0,
            is_labor: false,
            flat_total: Some(0.0),
            remark: None,
        }];
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let xml = read_xlsx_sheet1(&bytes);
        // G31 should contain 0 as a flat_total value
        let g31_pos = xml.find(r#"r="G31""#).unwrap();
        let g31_region = &xml[g31_pos..g31_pos + 200.min(xml.len() - g31_pos)];
        assert!(
            g31_region.contains("<v>0</v>"),
            "G31 should contain flat_total value 0, got: {}",
            g31_region
        );
        // Should be a number cell (t="n"), not a formula
        assert!(
            !g31_region.contains("<f>"),
            "G31 should not contain a formula for flat_total item"
        );
    }

    #[test]
    fn generate_with_detected_items_creates_second_sheet() {
        let mut data = minimal_offer_data();
        data.detected_items = vec![DetectedItemRow {
            name: "Sofa".to_string(),
            volume_m3: 1.5,
            dimensions: Some("200x90x85".to_string()),
            confidence: 0.9,
            german_name: None,
            re_value: None,
            volume_source: None,
            crop_s3_key: None,
            bbox: None,
            bbox_image_index: None,
            source_image_urls: None,
        }];
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let cursor = std::io::Cursor::new(&bytes);
        let mut archive = zip::ZipArchive::new(cursor).expect("valid zip");
        let names: Vec<String> = (0..archive.len())
            .map(|i| {
                let file = archive.by_index(i).unwrap();
                file.name().to_string()
            })
            .collect();
        assert!(
            names.iter().any(|n| n == "xl/worksheets/sheet2.xml"),
            "sheet2.xml should exist when detected_items is non-empty, found: {:?}",
            names
        );
    }

    #[test]
    fn generate_with_no_detected_items_has_no_second_sheet() {
        let data = minimal_offer_data(); // detected_items is empty
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let cursor = std::io::Cursor::new(&bytes);
        let mut archive = zip::ZipArchive::new(cursor).expect("valid zip");
        let has_sheet2 = (0..archive.len()).any(|i| {
            let file = archive.by_index(i).unwrap();
            file.name() == "xl/worksheets/sheet2.xml"
        });
        assert!(
            !has_sheet2,
            "sheet2.xml should NOT exist when detected_items is empty"
        );
    }

    #[test]
    fn multiple_line_items_fill_sequential_rows() {
        let mut data = minimal_offer_data();
        data.line_items = vec![
            OfferLineItem {
                description: "De/Montage".to_string(),
                quantity: 1.0,
                unit_price: 50.0,
                is_labor: false,
                flat_total: None,
                remark: None,
            },
            OfferLineItem {
                description: "Halteverbotszone".to_string(),
                quantity: 2.0,
                unit_price: 100.0,
                is_labor: false,
                flat_total: None,
                remark: None,
            },
            OfferLineItem {
                description: "Einpackservice".to_string(),
                quantity: 1.0,
                unit_price: 30.0,
                is_labor: false,
                flat_total: None,
                remark: None,
            },
        ];
        let bytes = generate_offer_xlsx(&data).expect("generate should succeed");
        let xml = read_xlsx_sheet1(&bytes);
        // Each line item should occupy rows 31, 32, 33 respectively
        // Check G31, G32, G33 all have formula cells
        assert!(xml.contains(r#"r="G31""#), "G31 should exist for item 1");
        assert!(xml.contains(r#"r="G32""#), "G32 should exist for item 2");
        assert!(xml.contains(r#"r="G33""#), "G33 should exist for item 3");
    }
}
