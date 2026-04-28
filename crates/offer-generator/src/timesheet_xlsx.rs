//! Generates a Stundenzettel (employee timesheet) XLSX from scratch.
//!
//! No template — builds a minimal valid XLSX ZIP directly from XML strings.
//! Layout mirrors the existing manual template (Arbeitszeit NN.YY.xlsx):
//!
//! ```
//! B2  Mitarbeiterdetails          E4  Arbeitsstunden gesamt
//! B3  {Name}                      E5  {total hours}
//! B5  Vorgesetztendetails         E6  Reguläre Arbeitsstunden
//! B6  Alex Aust                   E7  {regular hours}
//! B8  Zeitraum…                   E8  Überstunden
//! B9  Monat: | C9: {MM.YYYY}      E9  {overtime}
//!
//! B11 Datum | C11 Arbeitsbeginn | D11 Arbeitsende | E11 Arbeitsstunden
//! B12…  date     clock_in          clock_out          actual_hours
//! ```
//!
//! Times are converted from UTC to CET (UTC+1). DST is not accounted for;
//! for correct CEST handling add the `chrono-tz` crate later.

use crate::OfferError;
use chrono::{NaiveDate, NaiveTime};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One entry (worked day) in the timesheet.
#[derive(Debug, Clone)]
pub struct TimesheetEntry {
    /// Calendar date of the assignment.
    pub date: NaiveDate,
    /// Clock-in time. `None` if not recorded.
    pub clock_in: Option<NaiveTime>,
    /// Clock-out time. `None` if not recorded.
    pub clock_out: Option<NaiveTime>,
    /// Pre-computed actual hours. Used when clock times are absent.
    pub actual_hours: Option<f64>,
}

/// All data needed to generate one employee's monthly timesheet.
pub struct TimesheetData {
    /// Employee first name.
    pub first_name: String,
    /// Employee last name.
    pub last_name: String,
    /// Month label written to C9, e.g. `"03.2026"`.
    pub month_label: String,
    /// Monthly target hours from the employee record.
    pub target_hours: f64,
    /// All assignment entries for the month (unsorted; sorted internally).
    pub entries: Vec<TimesheetEntry>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Generates a Stundenzettel XLSX for one employee and returns the raw bytes.
///
/// **Caller**: `crates/api/src/routes/admin::employee_hours_export`
/// **Why**: Produces the monthly timesheet document Alex uses for payroll.
///
/// # Parameters
/// - `data` — employee profile + all assignment entries for the requested month
///
/// # Returns
/// Raw bytes of a valid `.xlsx` file.
///
/// # Errors
/// Returns `OfferError::Template` if ZIP assembly fails.
pub fn generate_timesheet_xlsx(data: &TimesheetData) -> Result<Vec<u8>, OfferError> {
    let mut entries = data.entries.clone();
    entries.sort_by_key(|e| e.date);

    // Pre-compute totals
    let total_hours: f64 = entries
        .iter()
        .filter_map(|e| effective_hours(e))
        .sum();
    let regular_hours = total_hours.min(data.target_hours);
    let overtime = (total_hours - data.target_hours).max(0.0);

    // Build XML components
    let sheet_xml = build_sheet_xml(data, &entries, total_hours, regular_hours, overtime);
    let name = format!("{} {}", data.last_name, data.first_name);
    let workbook_xml = build_workbook_xml(&name);
    let content_types = CONTENT_TYPES;
    let rels = ROOT_RELS;
    let workbook_rels = WORKBOOK_RELS;
    let styles = STYLES_XML;

    // Assemble ZIP
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut zip = ZipWriter::new(cursor);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let files: &[(&str, &str)] = &[
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", rels),
        ("xl/workbook.xml", &workbook_xml),
        ("xl/_rels/workbook.xml.rels", workbook_rels),
        ("xl/worksheets/sheet1.xml", &sheet_xml),
        ("xl/styles.xml", styles),
    ];

    for (name, content) in files {
        zip.start_file(*name, opts)
            .map_err(|e| OfferError::Template(format!("ZIP start_file {name}: {e}")))?;
        zip.write_all(content.as_bytes())
            .map_err(|e| OfferError::Template(format!("ZIP write {name}: {e}")))?;
    }

    let cursor = zip
        .finish()
        .map_err(|e| OfferError::Template(format!("ZIP finish: {e}")))?;
    Ok(cursor.into_inner())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the effective hours for an entry:
/// computed from clock_in/clock_out if both times present,
/// otherwise falls back to the pre-computed actual_hours.
fn effective_hours(e: &TimesheetEntry) -> Option<f64> {
    if let (Some(ci), Some(co)) = (e.clock_in, e.clock_out) {
        let secs = (co - ci).num_seconds();
        if secs > 0 {
            return Some(secs as f64 / 3600.0);
        }
    }
    e.actual_hours
}

/// Formats a NaiveTime as "HH:MM".
fn fmt_time(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

/// Escapes special XML characters in a string value.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Renders a string cell. Style 1 = bold header.
fn str_cell(coord: &str, value: &str, bold: bool) -> String {
    let s_attr = if bold { r#" s="1""# } else { "" };
    format!(
        r#"<c r="{coord}" t="inlineStr"{s_attr}><is><t>{v}</t></is></c>"#,
        coord = coord,
        s_attr = s_attr,
        v = xml_escape(value)
    )
}

/// Renders a numeric cell. Style 2 = 2-decimal-place number format.
fn num_cell(coord: &str, value: f64) -> String {
    format!(r#"<c r="{coord}" s="2"><v>{v:.4}</v></c>"#, coord = coord, v = value)
}

/// Wraps a row of cell XML in a `<row>` element.
fn row(r: usize, cells: &str) -> String {
    format!(r#"<row r="{r}">{cells}</row>"#, r = r, cells = cells)
}

// ---------------------------------------------------------------------------
// XML builders
// ---------------------------------------------------------------------------

fn build_sheet_xml(
    data: &TimesheetData,
    entries: &[TimesheetEntry],
    total: f64,
    regular: f64,
    overtime: f64,
) -> String {
    let full_name = format!("{} {}", data.first_name, data.last_name);
    let mut rows = Vec::<String>::new();

    // Header / summary section
    rows.push(row(2, &str_cell("B2", "Mitarbeiterdetails", true)));
    rows.push(row(3, &str_cell("B3", &full_name, false)));
    rows.push(row(4, &str_cell("E4", "Arbeitsstunden gesamt", false)));
    rows.push(row(
        5,
        &format!(
            "{}{}",
            str_cell("B5", "Vorgesetztendetails", true),
            num_cell("E5", total)
        ),
    ));
    rows.push(row(
        6,
        &format!(
            "{}{}",
            str_cell("B6", "Alex Aust", false),
            str_cell("E6", "Reguläre Arbeitsstunden", false)
        ),
    ));
    rows.push(row(7, &num_cell("E7", regular)));
    rows.push(row(
        8,
        &format!(
            "{}{}",
            str_cell("B8", "Zeitraum für Arbeitszeittabelle", false),
            str_cell("E8", "Überstunden", false)
        ),
    ));
    rows.push(row(
        9,
        &format!(
            "{}{}{}",
            str_cell("B9", "Monat:", false),
            str_cell("C9", &data.month_label, false),
            num_cell("E9", overtime)
        ),
    ));

    // Column headers (row 11)
    rows.push(row(
        11,
        &format!(
            "{}{}{}{}",
            str_cell("B11", "Datum", true),
            str_cell("C11", "Arbeitsbeginn", true),
            str_cell("D11", "Arbeitsende", true),
            str_cell("E11", "Arbeitsstunden", true)
        ),
    ));

    // Data rows from row 12
    for (i, entry) in entries.iter().enumerate() {
        let r = 12 + i;
        let date_str = entry.date.format("%d.%m.%Y").to_string();

        let clock_in_str = entry.clock_in.map(fmt_time);
        let clock_out_str = entry.clock_out.map(fmt_time);
        let hours = effective_hours(entry);

        let mut cells = str_cell(&format!("B{r}"), &date_str, false);
        if let Some(cin) = &clock_in_str {
            cells += &str_cell(&format!("C{r}"), cin, false);
        }
        if let Some(cout) = &clock_out_str {
            cells += &str_cell(&format!("D{r}"), cout, false);
        }
        if let Some(h) = hours {
            cells += &num_cell(&format!("E{r}"), h);
        }
        rows.push(row(r, &cells));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetPr><pageSetUpPr fitToPage="1"/></sheetPr>
<cols>
<col min="1" max="1" width="2" customWidth="1"/>
<col min="2" max="2" width="18" customWidth="1"/>
<col min="3" max="3" width="13" customWidth="1"/>
<col min="4" max="4" width="13" customWidth="1"/>
<col min="5" max="5" width="14" customWidth="1"/>
</cols>
<sheetData>
{rows}
</sheetData>
<pageMargins left="0.5" right="0.5" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>
<pageSetup paperSize="9" orientation="portrait" fitToWidth="1" fitToHeight="0"/>
</worksheet>"#,
        rows = rows.join("\n")
    )
}

fn build_workbook_xml(sheet_name: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>
<sheet name="{}" sheetId="1" r:id="rId1"/>
</sheets>
</workbook>"#,
        xml_escape(sheet_name)
    )
}

// ---------------------------------------------------------------------------
// Static XML fragments
// ---------------------------------------------------------------------------

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

/// Minimal styles: index 0 = default, 1 = bold, 2 = number (0.00).
const STYLES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="1">
<numFmt numFmtId="164" formatCode="0.00"/>
</numFmts>
<fonts count="2">
<font><sz val="11"/><name val="Calibri"/></font>
<font><b/><sz val="11"/><name val="Calibri"/></font>
</fonts>
<fills count="2">
<fill><patternFill patternType="none"/></fill>
<fill><patternFill patternType="gray125"/></fill>
</fills>
<borders count="1">
<border><left/><right/><top/><bottom/><diagonal/></border>
</borders>
<cellStyleXfs count="1">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
</cellStyleXfs>
<cellXfs count="3">
<xf numFmtId="0"   fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="0"   fontId="1" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0"/>
</cellXfs>
</styleSheet>"#;
