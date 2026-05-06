//! Direct XLSX template manipulation for invoice (Rechnung) generation.
//!
//! Uses the same XML-surgery approach as `xlsx.rs` (offer generator),
//! delegating cell-value writes, row hiding, and formula-cache stripping
//! to the shared functions exported from that module.
//!
//! ## Template
//!
//! The invoice generator uses `templates/Rechnung_Vorlage_v3.xlsx`, which is
//! derived from the manually created Rechnung layout (see `2025- Luttert.xlsx`).
//! The layout uses columns A–E with a clean invoice design:
//!
//! - **Address block**: A8–A11 (customer), A12 (contact)
//! - **Dates**:        C18/E18 labels, C19 service date, E19 invoice date
//! - **Title**:        A22 (e.g. "Rechnung Nr. 2026-0042")
//! - **Salutation**:   A24
//! - **Auftragsort**:  A26
//! - **Intro text**:   A27
//! - **Table header**: Row 30 (Pos., Beschreibung, Menge, Einzelpreis, Gesamtpreis)
//! - **Line items**:   rows 31–37 (A=pos, B=desc, C=qty, D=unit_price, E=formula D*C)
//! - **Totals**:       Row 38 Nettosumme (E38=SUM(E31:E37))
//!                     Row 39 zzgl. 19% MwSt. (E39=E38*19%)
//!                     Row 41 Rechnungsbetrag (E41=E38+E39)
//! - **Footer**:       Row 44 payment instruction, Row 47 "Mit freundlichen Grüßen",
//!                     Row 49 "Aust Umzüge & Haushaltsauflösungen"
//!
//! Formulas for line-item totals and the sums are pre-baked in the template.
//! The code only writes data cells (A, B, C, D) for items; the E-column formulas
//! are kept and recalculated by LibreOffice on load.
//!
//! ## Negative line items (Gutschrift / Anzahlungsabzug)
//!
//! `unit_price` may be negative. The arithmetic propagates correctly through
//! the D*C formula and SUM — no special handling needed. Negative values render
//! with the € currency format from the template's styles.

use crate::xlsx::{
    hide_row, set_cell_value, strip_formula_cached_values, unhide_row, CellValue,
};
use crate::OfferError;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// The invoice template XLSX — embedded at compile time.
///
/// `Rechnung_Vorlage_v4.xlsx` is the canonical Aust Rechnung layout
/// (sourced from `01 Rechnungen.xlsx`). Columns A–E, logo top-right,
/// info@/www in column E beside the customer block, formulas pre-baked.
const TEMPLATE_BYTES: &[u8] = include_bytes!("../../../templates/Rechnung_Vorlage_v4.xlsx");

/// First row used for line items in the invoice template.
const LINE_ITEM_START_ROW: u32 = 31;
/// Last row used for line items (rows 31–50 = 20 slots).
/// The totals block (rows 51–54) sits immediately after and must NOT be hidden.
const LINE_ITEM_END_ROW: u32 = 50;
/// Maximum number of line item rows.
const MAX_LINE_ITEMS: usize = 20;

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// All data needed to fill the Rechnung XLSX template for one invoice.
///
/// **Caller**: `crates/api/src/routes/invoices.rs`
/// **Why**: Single transfer object between the HTTP route and the XLSX generator.
///
/// Line items are the single source of truth for what appears on the invoice:
/// - **Full**: copied from offer line items + any on-site extras/credits
/// - **PartialFirst**: single Anzahlung line item
/// - **PartialFinal**: offer line items + extras + "Abzgl. Anzahlung" deduction
#[derive(Debug, Clone)]
pub struct InvoiceData {
    /// Invoice number printed in the heading, e.g. `"1-2026"`.
    pub invoice_number: String,
    /// Invoice type — determines the base line item description.
    pub invoice_type: InvoiceType,
    /// Invoice creation date.
    pub invoice_date: NaiveDate,
    /// Moving date (Leistungsdatum). `None` → blank.
    pub service_date: Option<NaiveDate>,
    /// Customer full name for the address block (A8).
    pub customer_name: String,
    /// Customer email address. Optional because some customers don't have email.
    pub customer_email: Option<String>,
    /// Company name for business customers — rendered above customer name in address block.
    pub company_name: Option<String>,
    /// Attention line for business customers, e.g. `"z.Hd. Herrn Schmidt"`.
    pub attention_line: Option<String>,
    /// Billing street + house number (A10).
    pub billing_street: String,
    /// Billing postal code + city, e.g. `"31135 Hildesheim"` (A11).
    pub billing_city: String,
    /// Offer number used in line item descriptions, e.g. `"2026-0042"`.
    pub offer_number: String,
    /// Formal salutation line, e.g. `"Sehr geehrter Herr Müller,"`.
    pub salutation: String,
    /// Line items for the invoice — the single source of truth.
    /// Each item has pos, description, quantity, unit_price (EUR, may be negative for credits),
    /// and optional remark. Maximum 7 items (rows 31–37).
    pub line_items: Vec<InvoiceLineItem>,

    // ── Legacy fields (kept for backward compatibility during migration) ──

    /// **Legacy**: Netto amount for the base line item in cents.
    /// Only used when `line_items` is empty (pre-migration invoices).
    #[deprecated(note = "Use line_items instead. base_netto_cents is derived from line items.")]
    pub base_netto_cents: i64,
    /// **Legacy**: Extra services appended as additional line items.
    /// Only used when `line_items` is empty (pre-migration invoices).
    #[deprecated(note = "Use line_items instead. Extra services are now part of line_items.")]
    pub extra_services: Vec<ExtraService>,
    /// **Legacy**: Origin street + house number (A10, A27).
    /// Only used when `billing_street` is empty.
    #[deprecated(note = "Use billing_street instead.")]
    pub origin_street: String,
    /// **Legacy**: Origin postal code + city (A11).
    /// Only used when `billing_city` is empty.
    #[deprecated(note = "Use billing_city instead.")]
    pub origin_city: String,
}

/// A single line item on an invoice.
///
/// **Caller**: `crates/api/src/routes/invoices.rs` — created from offer line items + on-site edits
/// **Why**: Invoices now itemise individual services (like offers), with optional on-site
/// extras (Zusatzleistungen) and credits/refunds (Gutschriften, negative unit_price).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvoiceLineItem {
    /// Position number displayed in column A (1, 2, 3, …).
    pub pos: u32,
    /// Description shown in column B, e.g. `"Umzugshelfer"` or `"Gutschrift: beschädigter Schrank"`.
    pub description: String,
    /// Quantity shown in column C (Menge). Default 1 for most services.
    pub quantity: f64,
    /// Netto unit price in euros (not cents!). May be negative for credits/refunds.
    /// Negative values propagate correctly through the template's D*C formula.
    pub unit_price: f64,
    /// Optional remark appended to the description, e.g. `"(Entladestelle)"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
}

/// A single extra service sold on-site.
///
/// **Legacy**: Kept for backward compatibility with pre-migration invoices.
/// New code should use `InvoiceLineItem` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraService {
    /// Short description shown in column B, e.g. `"Klaviertransport"`.
    pub description: String,
    /// Netto price in cents. Written to Einzelpreis (D column).
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

/// Generate a complete invoice XLSX from the embedded Rechnung_Vorlage_v3 template.
///
/// **Caller**: `crates/api/src/routes/invoices.rs` (create + regenerate paths)
/// **Why**: Fills the XLSX template with per-invoice data using the same XML-surgery
/// approach as the offer generator, delegating cell writes and row hiding to shared
/// functions from `xlsx.rs`.
///
/// The template uses columns A–E with:
/// - A: position number
/// - B: description (Beschreibung)
/// - C: quantity (Menge)
/// - D: unit price (Einzelpreis, may be negative for credits)
/// - E: total formula (=D*C, pre-baked in template)
///
/// Totals formulas (Nettosumme = SUM, MwSt = 19%, Rechnungsbetrag = sum) are
/// pre-baked in the template. The code only writes data cells.
///
/// **Negative line items**: `unit_price` may be negative (Gutschrift / Anzahlungsabzug).
/// Negative values flow through the D*C formula and SUM without any special handling.
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

    // Build cell modifications and determine which rows are used
    let (cell_mods, hidden_rows, unhidden_rows) = build_cell_modifications(data);

    // Read sheet1.xml
    let sheet1_xml = read_zip_entry(&mut template_zip, "xl/worksheets/sheet1.xml")?;
    let sheet1_str = String::from_utf8(sheet1_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml not valid UTF-8: {e}")))?;

    // Apply cell modifications
    let mut modified_sheet1 = sheet1_str;
    for (cell_ref, value) in &cell_mods {
        modified_sheet1 = set_cell_value(&modified_sheet1, cell_ref, value);
    }

    // Hide/unhide line-item rows
    for &row in &hidden_rows {
        modified_sheet1 = hide_row(&modified_sheet1, row);
    }
    for &row in &unhidden_rows {
        modified_sheet1 = unhide_row(&modified_sheet1, row);
    }

    // Strip stale formula caches so LibreOffice recalculates on open
    modified_sheet1 = strip_formula_cached_values(&modified_sheet1);

    // Remove hyperlinks section — email as plain text
    modified_sheet1 = strip_hyperlinks(&modified_sheet1);

    // Read workbook.xml and force recalculation on load + fix print area
    let workbook_xml = read_zip_entry(&mut template_zip, "xl/workbook.xml")?;
    let workbook_str = String::from_utf8(workbook_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml not valid UTF-8: {e}")))?;
    let modified_workbook = modify_invoice_workbook(&workbook_str);

    // Read sheet1 rels and strip hyperlink relationships (keep drawing rel)
    let sheet1_rels_xml =
        read_zip_entry(&mut template_zip, "xl/worksheets/_rels/sheet1.xml.rels")?;
    let sheet1_rels_str = String::from_utf8(sheet1_rels_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml.rels not valid UTF-8: {e}")))?;
    let modified_sheet1_rels = strip_hyperlink_rels(&sheet1_rels_str);

    // Read drawing1.xml and strip any Rechnung-specific shapes (none in v3 template)
    let drawing_xml = read_zip_entry(&mut template_zip, "xl/drawings/drawing1.xml")?;
    let drawing_str = String::from_utf8(drawing_xml)
        .map_err(|e| OfferError::Template(format!("drawing1.xml not valid UTF-8: {e}")))?;
    let modified_drawing = strip_rechnung_drawing(&drawing_str);

    // Reassemble ZIP
    assemble_invoice_xlsx(
        &mut template_zip,
        &modified_sheet1,
        &modified_workbook,
        &modified_sheet1_rels,
        &modified_drawing,
    )
}

// ---------------------------------------------------------------------------
// Cell modification builder
// ---------------------------------------------------------------------------

/// Build the list of cell value changes for this invoice, plus row visibility lists.
///
/// Returns `(cell_modifications, hidden_rows, unhidden_rows)`.
///
/// Template layout (columns A–E):
/// - A: Position number
/// - B: Beschreibung (description)
/// - C: Menge (quantity)
/// - D: Einzelpreis (unit price, may be negative for credits)
/// - E: Gesamt Netto (formula D*C, pre-baked in template)
///
/// Totals block (rows 38–41):
/// - C38 / E38: Nettosumme / SUM(E31:E37)
/// - C39 / E39: zzgl. 19% MwSt. / E38*19%
/// - C41 / E41: Rechnungsbetrag / E38+E39
///
/// Footer (rows 44–49): already present as shared strings in the template.
fn build_cell_modifications(
    data: &InvoiceData,
) -> (Vec<(String, CellValue)>, Vec<u32>, Vec<u32>) {
    let mut mods = Vec::new();

    // Resolve effective address fields (prefer new, fall back to legacy)
    let billing_street = if data.billing_street.is_empty() {
        #[allow(deprecated)]
        data.origin_street.clone()
    } else {
        data.billing_street.clone()
    };
    let billing_city = if data.billing_city.is_empty() {
        #[allow(deprecated)]
        data.origin_city.clone()
    } else {
        data.billing_city.clone()
    };

    // ── Address block (A8–A11) ────────────────────────────────────────────
    // A8 reserved for the company name (blank on private invoices) so the
    // contact name in A9 sits in the same line whether or not it's a business.
    // Email is intentionally NOT written to the invoice (data privacy).
    let (a8, a9) = if let Some(ref company) = data.company_name {
        let attn = data
            .attention_line
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| data.customer_name.clone());
        (company.clone(), attn)
    } else {
        (String::new(), data.customer_name.clone())
    };
    mods.push(("A8".into(), CellValue::Text(a8)));
    mods.push(("A9".into(), CellValue::Text(a9)));
    mods.push(("A10".into(), CellValue::Text(billing_street.clone())));
    mods.push(("A11".into(), CellValue::Text(billing_city.clone())));
    // A12 historically held an "E-Mail: …" line; cleared for privacy.
    mods.push(("A12".into(), CellValue::Text(String::new())));

    // ── Dates (C18–C19, E18–E19) ─────────────────────────────────────────
    let service_date_str = data
        .service_date
        .map(|d| d.format("%d.%m.%Y").to_string())
        .unwrap_or_default();
    mods.push(("C19".into(), CellValue::Text(service_date_str)));

    // Write invoice date to E19 (replaces the TODAY() formula)
    mods.push((
        "E19".into(),
        CellValue::Text(data.invoice_date.format("%d.%m.%Y").to_string()),
    ));

    // ── Title: invoice number (A22) ────────────────────────────────────────
    mods.push((
        "A22".into(),
        CellValue::Text(format!("Rechnung Nr. {}", data.invoice_number)),
    ));

    // ── Salutation / greeting (A24) ────────────────────────────────────────
    mods.push(("A24".into(), CellValue::Text(data.salutation.clone())));

    // ── Intro text (A26) ──────────────────────────────────────────────────
    mods.push((
        "A26".into(),
        CellValue::Text(
            "wir bedanken uns für Ihr Vertrauen und stellen Ihnen vereinbarungsgemäß folgendes in Rechnung.".into(),
        ),
    ));

    // ── Auftragsort (A27) ──────────────────────────────────────────────────
    mods.push((
        "A27".into(),
        CellValue::Text(format!("Auftragsort: {}, {}", billing_street, billing_city)),
    ));

    // ── Line items (rows 31–37, columns A-D) ──────────────────────────────
    // Column E formulas (D*C) are pre-baked in the template — do not touch.
    //
    // The template has alternating styles per row via style indices.
    // We use plain CellValue::Text/Number so the existing template style is preserved.

    // 1. Hide ALL line-item rows and pre-clear C/D to avoid stale preset values
    let mut hidden_rows: Vec<u32> = (LINE_ITEM_START_ROW..=LINE_ITEM_END_ROW).collect();
    let mut unhidden_rows: Vec<u32> = Vec::new();

    for row in LINE_ITEM_START_ROW..=LINE_ITEM_END_ROW {
        mods.push((format!("C{row}"), CellValue::Number(0.0)));
        mods.push((format!("D{row}"), CellValue::Number(0.0)));
        // Clear B column (description) so hidden rows don't show stale text
        mods.push((format!("B{row}"), CellValue::Text(String::new())));
    }

    // 2. Resolve effective line items: prefer new field, fall back to legacy model
    let items_owned: Vec<InvoiceLineItem>;
    #[allow(deprecated)]
    let effective_items: &[InvoiceLineItem] = if !data.line_items.is_empty() {
        &data.line_items
    } else {
        items_owned = build_legacy_line_items(data);
        &items_owned
    };

    if effective_items.len() > MAX_LINE_ITEMS {
        tracing::warn!(
            invoice_number = %data.invoice_number,
            total_items = effective_items.len(),
            max_items = MAX_LINE_ITEMS,
            "Invoice has {} line items but template only has {} slots (rows {}-{}). \
             Excess items will be truncated in PDF.",
            effective_items.len(),
            MAX_LINE_ITEMS,
            LINE_ITEM_START_ROW,
            LINE_ITEM_END_ROW,
        );
    }

    // 3. Write items sequentially
    for (i, item) in effective_items.iter().take(MAX_LINE_ITEMS).enumerate() {
        let row = LINE_ITEM_START_ROW + i as u32;
        unhidden_rows.push(row);

        // A: Position number (shown as "1.", "2.", etc.)
        mods.push((
            format!("A{row}"),
            CellValue::Text(format!("{}.", item.pos)),
        ));

        // B: Description (with optional remark appended)
        let desc = match &item.remark {
            Some(remark) if !remark.is_empty() => {
                format!("{} ({})", item.description, remark)
            }
            _ => item.description.clone(),
        };
        mods.push((format!("B{row}"), CellValue::Text(desc)));

        // C: Menge (quantity)
        mods.push((format!("C{row}"), CellValue::Number(item.quantity)));

        // D: Einzelpreis (unit price, may be negative)
        mods.push((format!("D{row}"), CellValue::Number(item.unit_price)));

        // E: DO NOT write — the template already has D{row}*C{row} formula.
        //    strip_formula_cached_values will clear stale cache.
    }

    // Remove unhidden rows from hidden list
    hidden_rows.retain(|r| !unhidden_rows.contains(r));

    // ── Totals block (rows 38–41) ─────────────────────────────────────────
    // C38 = "Nettosumme" (shared string, keep as-is)
    // E38 = SUM(E31:E37) (formula, keep as-is)
    // C39 = "zzgl. 19% MwSt." (shared string, keep as-is)
    // E39 = E38*19% (formula, keep as-is)
    // C41 = "Rechnungsbetrag" (shared string, keep as-is)
    // E41 = E38+E39 (formula, keep as-is)
    //
    // All labels and formulas are pre-baked in the template. strip_formula_cached_values
    // will handle clearing stale caches so LibreOffice recalculates.

    // ── Footer (rows 44–49) ────────────────────────────────────────────────
    // A44 = payment instruction text (shared string, keep as-is)
    // A47 = "Mit freundlichen Grüßen" (shared string, keep as-is)
    // A49 = "Aust Umzüge & Haushaltsauflösungen" (shared string, keep as-is)
    //
    // All footer text is pre-baked in the template as shared strings.

    (mods, hidden_rows, unhidden_rows)
}

/// Build line items from the legacy `base_netto_cents` + `extra_services` model.
#[allow(deprecated)]
fn build_legacy_line_items(data: &InvoiceData) -> Vec<InvoiceLineItem> {
    let mut items = Vec::new();

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
    items.push(InvoiceLineItem {
        pos: 1,
        description: base_description,
        quantity: 1.0,
        unit_price: base_netto_euros,
        remark: None,
    });

    for (i, extra) in data.extra_services.iter().take(7).enumerate() {
        let extra_euros = extra.price_cents as f64 / 100.0;
        items.push(InvoiceLineItem {
            pos: (i + 2) as u32,
            description: extra.description.clone(),
            quantity: 1.0,
            unit_price: extra_euros,
            remark: None,
        });
    }

    items
}

// ---------------------------------------------------------------------------
// Workbook modification
// ---------------------------------------------------------------------------

/// Patch `workbook.xml`: fix print area to A:E and ensure fullCalcOnLoad.
///
/// The v4 template's print area covers A1:E69 (20 line item rows + totals + footer).
fn modify_invoice_workbook(xml: &str) -> String {
    let mut result = xml.to_string();

    // Fix print area to A:E covering line items + totals + footer
    if let Some(start) = result.find("_xlnm.Print_Area") {
        if let Some(content_start) = result[start..].find('>') {
            let abs_content_start = start + content_start + 1;
            if let Some(end_tag) = result[abs_content_start..].find("</definedName>") {
                let abs_end = abs_content_start + end_tag;
                let mut patched = String::with_capacity(result.len());
                patched.push_str(&result[..abs_content_start]);
                patched.push_str("Tabelle1!$A$1:$E$69");
                patched.push_str(&result[abs_end..]);
                result = patched;
            }
        }
    }

    // Ensure fullCalcOnLoad is set
    if result.contains(r#"fullCalcOnLoad="false""#) {
        result = result.replace(r#"fullCalcOnLoad="false""#, r#"fullCalcOnLoad="true""#);
    } else if !result.contains("fullCalcOnLoad=") {
        if let Some(pos) = result.find("<calcPr") {
            if let Some(gt) = result[pos..].find("/>") {
                let insert_at = pos + gt;
                result.insert_str(insert_at, r#" fullCalcOnLoad="true""#);
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Drawing shape stripping
// ---------------------------------------------------------------------------

/// No AGB shapes to strip in the v3 Rechnung template, but keep this function
/// as a safety net in case a future template adds unwanted drawing shapes.
fn strip_rechnung_drawing(xml: &str) -> String {
    // The v3 template only has the logo drawing — keep everything.
    xml.to_string()
}

// ---------------------------------------------------------------------------
// Hyperlink stripping
// ---------------------------------------------------------------------------

/// Remove the `<hyperlinks>` block from `sheet1.xml` so email addresses render
/// as plain text rather than clickable hyperlinks.
fn strip_hyperlinks(xml: &str) -> String {
    if let Some(start) = xml.find("<hyperlinks>") {
        if let Some(end) = xml.find("</hyperlinks>") {
            let mut result = String::with_capacity(xml.len());
            result.push_str(&xml[..start]);
            result.push_str(&xml[end + "</hyperlinks>".len()..]);
            return result;
        }
    }
    xml.to_string()
}

/// Remove hyperlink `<Relationship>` entries from `sheet1.xml.rels`, keeping
/// the drawing relationship.
fn strip_hyperlink_rels(rels_xml: &str) -> String {
    let mut result = String::with_capacity(rels_xml.len());
    let mut pos = 0;
    while pos < rels_xml.len() {
        if let Some(rel_start) = rels_xml[pos..].find("<Relationship ") {
            let abs_start = pos + rel_start;
            if let Some(rel_end) = rels_xml[abs_start..].find("/>") {
                let abs_end = abs_start + rel_end + 2;
                let rel_fragment = &rels_xml[abs_start..abs_end];
                if rel_fragment.contains("hyperlink") {
                    result.push_str(&rels_xml[pos..abs_start]);
                    pos = abs_end;
                    continue;
                }
            }
        }
        result.push_str(&rels_xml[pos..]);
        break;
    }
    result
}

// ---------------------------------------------------------------------------
// ZIP utilities
// ---------------------------------------------------------------------------

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

fn assemble_invoice_xlsx(
    template_zip: &mut ZipArchive<Cursor<&'static [u8]>>,
    modified_sheet1: &str,
    modified_workbook: &str,
    modified_sheet1_rels: &str,
    modified_drawing: &str,
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
            "xl/worksheets/_rels/sheet1.xml.rels" => {
                writer
                    .write_all(modified_sheet1_rels.as_bytes())
                    .map_err(|e| {
                        OfferError::Template(format!("Failed to write sheet1.xml.rels: {e}"))
                    })?;
            }
            "xl/drawings/drawing1.xml" => {
                writer
                    .write_all(modified_drawing.as_bytes())
                    .map_err(|e| OfferError::Template(format!("Failed to write drawing1.xml: {e}")))?;
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data() -> InvoiceData {
        #[allow(deprecated)]
        InvoiceData {
            invoice_number: "2026-0001".into(),
            invoice_type: InvoiceType::Full,
            invoice_date: NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
            customer_name: "Max Mustermann".into(),
            customer_email: Some("max@example.com".into()),
            company_name: None,
            attention_line: None,
            billing_street: "Musterstraße 1".into(),
            billing_city: "31135 Hildesheim".into(),
            offer_number: "2026-0042".into(),
            salutation: "Sehr geehrter Herr Mustermann,".into(),
            line_items: vec![
                InvoiceLineItem {
                    pos: 1,
                    description: "Umzugsdienstleistung".into(),
                    quantity: 1.0,
                    unit_price: 1000.0,
                    remark: None,
                },
                InvoiceLineItem {
                    pos: 2,
                    description: "Gutschrift: beschädigter Schrank".into(),
                    quantity: 1.0,
                    unit_price: -50.0,
                    remark: Some("Anzahlungsabzug".into()),
                },
            ],
            base_netto_cents: 0,
            extra_services: vec![],
            origin_street: String::new(),
            origin_city: String::new(),
        }
    }

    #[test]
    fn test_generate_invoice_xlsx_returns_bytes() {
        let data = sample_data();
        let result = generate_invoice_xlsx(&data);
        assert!(result.is_ok(), "generate_invoice_xlsx failed: {:?}", result.err());
        let bytes = result.unwrap();
        assert!(bytes.len() > 1000, "XLSX output suspiciously small: {} bytes", bytes.len());
        // Verify it's a valid ZIP (XLSX = ZIP)
        assert_eq!(&bytes[0..4], b"PK\x03\x04", "Output is not a valid ZIP/XLSX");
    }

    #[test]
    fn test_negative_unit_price_does_not_panic() {
        let data = sample_data();
        let result = generate_invoice_xlsx(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_partial_first_invoice() {
        #[allow(deprecated)]
        let data = InvoiceData {
            invoice_number: "2026-0002".into(),
            invoice_type: InvoiceType::PartialFirst { percent: 30 },
            invoice_date: NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            service_date: None,
            customer_name: "Erika Musterfrau".into(),
            customer_email: None,
            company_name: None,
            attention_line: None,
            billing_street: "Bahnhofstr. 5".into(),
            billing_city: "30159 Hannover".into(),
            offer_number: "2026-0010".into(),
            salutation: "Sehr geehrte Frau Musterfrau,".into(),
            line_items: vec![InvoiceLineItem {
                pos: 1,
                description: "Anzahlung (30%) — gemäß Angebot Nr. 2026-0010".into(),
                quantity: 1.0,
                unit_price: 357.14,
                remark: None,
            }],
            base_netto_cents: 0,
            extra_services: vec![],
            origin_street: String::new(),
            origin_city: String::new(),
        };
        let result = generate_invoice_xlsx(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_legacy_line_items_fallback() {
        #[allow(deprecated)]
        let data = InvoiceData {
            invoice_number: "2026-0003".into(),
            invoice_type: InvoiceType::Full,
            invoice_date: NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
            service_date: None,
            customer_name: "Legacy Customer".into(),
            customer_email: None,
            company_name: None,
            attention_line: None,
            billing_street: "Altgasse 7".into(),
            billing_city: "12345 Altstadt".into(),
            offer_number: "2025-0099".into(),
            salutation: "Sehr geehrte Damen und Herren,".into(),
            line_items: vec![], // empty → triggers legacy path
            base_netto_cents: 84034,
            extra_services: vec![ExtraService {
                description: "Klaviertransport".into(),
                price_cents: 15000,
            }],
            origin_street: String::new(),
            origin_city: String::new(),
        };
        let result = generate_invoice_xlsx(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_max_line_items_no_panic() {
        #[allow(deprecated)]
        let mut data = sample_data();
        // Fill 10 items — should truncate to 7, not panic
        data.line_items = (1..=10)
            .map(|i| InvoiceLineItem {
                pos: i,
                description: format!("Posten {i}"),
                quantity: 1.0,
                unit_price: 100.0 * i as f64,
                remark: None,
            })
            .collect();
        let result = generate_invoice_xlsx(&data);
        assert!(result.is_ok());
    }

    #[allow(deprecated)]
    fn realistic_data() -> InvoiceData {
        InvoiceData {
            invoice_number: "2026-TEST".into(),
            invoice_type: InvoiceType::Full,
            invoice_date: NaiveDate::from_ymd_opt(2026, 4, 25).unwrap(),
            service_date: Some(NaiveDate::from_ymd_opt(2026, 4, 22).unwrap()),
            customer_name: "Siggi Karge".into(),
            customer_email: Some("siggi.karge@gmail.com".into()),
            company_name: None,
            attention_line: None,
            billing_street: "Kirchweg 6".into(),
            billing_city: "31162 Bad Salzdetfurth".into(),
            offer_number: "2026-0006".into(),
            salutation: "Sehr geehrter Herr Karge,".into(),
            line_items: vec![
                InvoiceLineItem { pos: 1, description: "2 Umzugshelfer".into(), quantity: 1.0, unit_price: 90.0, remark: None },
                InvoiceLineItem { pos: 2, description: "3,5t Transporter m. Koffer".into(), quantity: 1.0, unit_price: 60.0, remark: Some("m. Koffer".into()) },
                InvoiceLineItem { pos: 3, description: "Gutschrift, Kaput TV".into(), quantity: 1.0, unit_price: -50.0, remark: None },
                InvoiceLineItem { pos: 4, description: "Haey".into(), quantity: 1.0, unit_price: 10.0, remark: None },
            ],
            base_netto_cents: 0,
            extra_services: vec![],
            origin_street: String::new(),
            origin_city: String::new(),
        }
    }

    #[tokio::test]
    async fn test_generate_realistic_invoice_pdf() {
        let data = realistic_data();
        let xlsx = generate_invoice_xlsx(&data).expect("XLSX generation failed");
        std::fs::write("/tmp/test_invoice.xlsx", &xlsx).unwrap();
        println!("XLSX: /tmp/test_invoice.xlsx");
        match crate::pdf_convert::convert_xlsx_to_pdf(&xlsx).await {
            Ok(pdf) => {
                let path = "/home/timefliez/Desktop/test_invoice_output.pdf";
                std::fs::write(path, pdf).unwrap();
                println!("PDF: {path}");
            }
            Err(e) => panic!("PDF conversion failed: {e}"),
        }
    }
}
