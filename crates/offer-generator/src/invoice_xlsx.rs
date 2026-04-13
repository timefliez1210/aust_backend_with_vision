//! Direct XLSX template manipulation for invoice (Rechnung) generation.
//!
//! Uses the same XML-surgery approach as `xlsx.rs` (offer generator),
//! delegating cell-value writes, row hiding, and formula-cache stripping
//! to the shared functions exported from that module.
//!
//! Invoice template layout (Rechnung_Vorlage.xlsx):
//! - 5 columns: Pos. | Bezeichnung | Menge | Einzelpreis | Gesamtpreis
//! - 16 line item slots (rows 31–46), row-hiding for unused rows
//! - Totals: E47 = SUM(E31:E46) Nettosumme, E48 = 19% MwSt, E50 = Bruttobetrag
//!
//! Invoice types and their line items:
//! - **Full**: offer line items + any Zusatzleistungen / Gutschriften
//! - **PartialFirst**: single "Anzahlung (X%)" line
//! - **PartialFinal**: offer line items + extras + "Abzgl. Anzahlung" deduction

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
const TEMPLATE_BYTES: &[u8] = include_bytes!("../../../templates/Rechnung_Vorlage.xlsx");

/// First row used for line items in the invoice template.
const LINE_ITEM_START_ROW: u32 = 31;
/// Maximum number of line item rows in the invoice template (rows 31–46).
const MAX_LINE_ITEMS: usize = 16;

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
    /// Invoice creation date written to E19 (replaces `=TODAY()` formula).
    pub invoice_date: NaiveDate,
    /// Moving date written to C19 (Leistungsdatum). `None` → blank.
    pub service_date: Option<NaiveDate>,
    /// Customer full name for the address block (A8).
    pub customer_name: String,
    /// Customer email address (A9). Optional because some customers don't have email.
    pub customer_email: Option<String>,
    /// Company name for business customers — rendered above customer name in the address block.
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
    /// and optional remark. Maximum 16 items (rows 31–46).
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
    /// Description shown in column B, e.g. `"De/Montage"` or `"Gutschrift: beschädigter Schrank"`.
    pub description: String,
    /// Quantity shown in column C. Default 1 for most services.
    pub quantity: f64,
    /// Netto unit price in euros (not cents!). May be negative for credits/refunds.
    pub unit_price: f64,
    /// Optional remark appended after description, e.g. `"Entladestelle"`.
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

/// Generate a complete invoice XLSX from the embedded Rechnung template.
///
/// **Caller**: `crates/api/src/routes/invoices.rs` (create + regenerate paths)
/// **Why**: Fills the XLSX template with per-invoice data using the same XML-surgery
/// approach as the offer generator, delegating cell writes and row hiding to shared functions.
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
    let (cell_mods, used_rows) = build_cell_modifications(data);

    // Read sheet1.xml
    let sheet1_xml = read_zip_entry(&mut template_zip, "xl/worksheets/sheet1.xml")?;
    let sheet1_str = String::from_utf8(sheet1_xml)
        .map_err(|e| OfferError::Template(format!("sheet1.xml not valid UTF-8: {e}")))?;

    // Apply cell modifications
    let mut modified_sheet1 = sheet1_str;
    for (cell_ref, value) in &cell_mods {
        modified_sheet1 = set_cell_value(&modified_sheet1, cell_ref, value);
    }

    // Hide unused line-item rows (31–46) — mirrors the offer template approach
    for row in LINE_ITEM_START_ROW..(LINE_ITEM_START_ROW + MAX_LINE_ITEMS as u32) {
        if !used_rows.contains(&row) {
            modified_sheet1 = hide_row(&modified_sheet1, row);
        } else {
            // Ensure used rows are NOT hidden (in case template had them hidden)
            modified_sheet1 = unhide_row(&modified_sheet1, row);
        }
    }

    // Strip stale formula caches so LibreOffice recalculates on open
    modified_sheet1 = strip_formula_cached_values(&modified_sheet1);

    // Read workbook.xml and force recalculation on load
    let workbook_xml = read_zip_entry(&mut template_zip, "xl/workbook.xml")?;
    let workbook_str = String::from_utf8(workbook_xml)
        .map_err(|e| OfferError::Template(format!("workbook.xml not valid UTF-8: {e}")))?;
    let modified_workbook = force_recalc_on_load(&workbook_str);

    // Reassemble ZIP
    assemble_invoice_xlsx(&mut template_zip, &modified_sheet1, &modified_workbook)
}

// ---------------------------------------------------------------------------
// Cell modification builder
// ---------------------------------------------------------------------------

/// Build the list of cell value changes for this invoice, plus the list of used rows.
///
/// Returns `(cell_modifications, used_row_numbers)`.
fn build_cell_modifications(data: &InvoiceData) -> (Vec<(String, CellValue)>, Vec<u32>) {
    let mut mods = Vec::new();
    let mut used_rows = Vec::new();

    // Resolve effective values: prefer new fields, fall back to legacy fields
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

    // Address block: for business, A8=company A9=attention; for private, A8=name A9=email or fallback
    if let Some(ref company) = data.company_name {
        mods.push(("A8".into(), CellValue::Text(company.clone())));
        let attn = data.attention_line.clone().unwrap_or_default();
        mods.push(("A9".into(), CellValue::Text(if attn.is_empty() { data.customer_email.as_deref().unwrap_or("").to_string() } else { attn })));
    } else {
        mods.push(("A8".into(), CellValue::Text(data.customer_name.clone())));
        mods.push(("A9".into(), CellValue::Text(data.customer_email.clone().unwrap_or_default())));
    }
    mods.push(("A10".into(), CellValue::Text(billing_street.clone())));
    mods.push(("A11".into(), CellValue::Text(billing_city.clone())));

    // Dates
    let service_date_str = data.service_date
        .map(|d| d.format("%d.%m.%Y").to_string())
        .unwrap_or_default();
    mods.push(("C19".into(), CellValue::Text(service_date_str)));
    mods.push(("E19".into(), CellValue::Text(data.invoice_date.format("%d.%m.%Y").to_string())));

    // Invoice number heading (A22)
    mods.push(("A22".into(), CellValue::Text(format!("Rechnung Nr.{}", data.invoice_number))));

    // Salutation (A24)
    mods.push(("A24".into(), CellValue::Text(data.salutation.clone())));

    // Job location (A27)
    mods.push(("A27".into(), CellValue::Text(format!("Auftragsort:  {}, {}", billing_street, billing_city))));

    // ── Line items ────────────────────────────────────────────────────────
    let items: Vec<InvoiceLineItem>;
    #[allow(deprecated)]
    let effective_items: &[InvoiceLineItem] = if !data.line_items.is_empty() {
        &data.line_items
    } else {
        items = build_legacy_line_items(data);
        &items
    };

    for (i, item) in effective_items.iter().take(MAX_LINE_ITEMS).enumerate() {
        let row = LINE_ITEM_START_ROW + i as u32;
        used_rows.push(row);

        // Column A: position number
        mods.push((format!("A{row}"), CellValue::Text(format!("{}.", item.pos))));
        // Column B: description (with remark in parentheses if present)
        let desc = match &item.remark {
            Some(r) if !r.is_empty() => format!("{} ({})", item.description, r),
            _ => item.description.clone(),
        };
        mods.push((format!("B{row}"), CellValue::Text(desc)));
        // Column C: quantity
        mods.push((format!("C{row}"), CellValue::Number(item.quantity)));
        // Column D: unit price in EUR (may be negative for credits)
        mods.push((format!("D{row}"), CellValue::Number(item.unit_price)));
    }

    (mods, used_rows)
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

    for (i, extra) in data.extra_services.iter().take(6).enumerate() {
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

/// Add `fullCalcOnLoad="1"` to `<calcPr>` in `workbook.xml`.
fn force_recalc_on_load(workbook_xml: &str) -> String {
    if workbook_xml.contains(r#"fullCalcOnLoad="0""#) {
        return workbook_xml.replace(r#"fullCalcOnLoad="0""#, r#"fullCalcOnLoad="1""#);
    }
    if workbook_xml.contains("fullCalcOnLoad=") {
        return workbook_xml.to_string();
    }
    if let Some(pos) = workbook_xml.find("<calcPr") {
        let after = &workbook_xml[pos..];
        if let Some(gt) = after.find('>') {
            let slash_offset = if gt > 0 && after.as_bytes()[gt - 1] == b'/' { 1 } else { 0 };
            let insert_at = pos + gt - slash_offset;
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