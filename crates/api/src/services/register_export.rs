//! Rechnungsausgangsbuch → XLSX.
//!
//! The register is what Alex hands to the Steuerberater, who has received an .xlsx
//! every year. This writes that file directly: same columns, same order, same German
//! formatting as the sheet he used to maintain by hand.
//!
//! # Why a hand-rolled writer
//! An XLSX is a ZIP of XML parts. We need six small static-ish parts and one sheet, so
//! the `zip` crate (already a workspace dependency, used the same way by
//! `offer-generator`) is enough — no spreadsheet library is pulled in for one table.
//!
//! Dates and money are written as real numbers with a display format, not as text, so
//! the Steuerberater can sum and filter the sheet. Everything else is an inline string;
//! there is no shared-string table to keep consistent.

use std::io::{Cursor, Write};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use zip::{write::SimpleFileOptions, ZipWriter};

use crate::ApiError;

/// One register row, flattened to exactly what the sheet prints.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ExportRow {
    pub invoice_number: String,
    /// Leistungszeitraum, pre-rendered: `"24.02.2026"` or `"12.-13.01.2026"`.
    pub service_period: String,
    pub customer: String,
    pub netto_cents: Option<i64>,
    pub mwst_cents: Option<i64>,
    pub brutto_cents: Option<i64>,
    pub sent_at: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub paid_at: Option<NaiveDate>,
    /// Alex's "Offene Zahlungen" column: the literal word `Bezahlt`, or the amount due.
    pub offen_cents: Option<i64>,
    pub payment_method: String,
    pub notes: String,
}

/// Render a Leistungszeitraum the way Alex writes it.
///
/// A single day is just that day. A span within one month collapses the repeated
/// month/year (`"12.-13.01.2026"`), which is the form 20 of his 26 span rows use;
/// anything wider prints both dates in full.
pub(crate) fn format_service_period(start: Option<NaiveDate>, end: Option<NaiveDate>) -> String {
    let Some(start) = start else { return String::new() };
    let Some(end) = end.filter(|e| *e != start) else {
        return start.format("%d.%m.%Y").to_string();
    };
    // An end before the start is bad data, not a span — show the start alone rather
    // than printing a backwards range.
    if end < start {
        return start.format("%d.%m.%Y").to_string();
    }
    if start.month() == end.month() && start.year() == end.year() {
        format!("{}.-{}", start.format("%d"), end.format("%d.%m.%Y"))
    } else {
        format!("{}-{}", start.format("%d.%m.%Y"), end.format("%d.%m.%Y"))
    }
}

/// Excel's day zero is 1899-12-30 (the epoch its own 1900 leap-year bug implies).
const EXCEL_EPOCH: Option<NaiveDate> = NaiveDate::from_ymd_opt(1899, 12, 30);

fn excel_serial(d: NaiveDate) -> i64 {
    EXCEL_EPOCH.map_or(0, |epoch| (d - epoch).num_days())
}

/// The date part of a timestamp in Europe/Berlin.
///
/// The register prints German dates, and a payment booked at 23:30 Berlin time is
/// 21:30 UTC the same day but 00:30 UTC the *next* day in winter — reading the UTC
/// date directly would move it.
pub(crate) fn berlin_date(ts: DateTime<Utc>) -> NaiveDate {
    ts.with_timezone(&chrono_tz::Europe::Berlin).date_naive()
}

// ── XML helpers ─────────────────────────────────────────────────────────────

/// Escape text for an XML text node and strip characters XML 1.0 cannot carry.
///
/// Customer names and Alex's Bemerkungen are free text — an `&` in
/// "erfi Ernst Fischer GmbH+Co.KG & Söhne" must not produce a file Excel refuses
/// to open.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Legal XML 1.0 characters only; anything else (stray control bytes from
            // a pasted email) is dropped rather than corrupting the part.
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// `A1`-style reference for a 0-based column and 1-based row.
fn cell_ref(col: usize, row: usize) -> String {
    let mut name = String::new();
    let mut n = col + 1;
    while n > 0 {
        let rem = (n - 1) % 26;
        name.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    format!("{name}{row}")
}

/// Style indices into the `cellXfs` table written by [`styles_xml`].
mod style {
    pub const PLAIN: u32 = 0;
    pub const HEADER: u32 = 1;
    pub const DATE: u32 = 2;
    pub const MONEY: u32 = 3;
    pub const TITLE: u32 = 4;
    pub const TOTAL_TEXT: u32 = 5;
    pub const TOTAL_MONEY: u32 = 6;
}

enum Cell {
    Empty,
    Text(String),
    Number(f64),
    Date(NaiveDate),
}

fn cell_xml(col: usize, row: usize, cell: &Cell, style: u32) -> String {
    let r = cell_ref(col, row);
    match cell {
        Cell::Empty => format!(r#"<c r="{r}" s="{style}"/>"#),
        Cell::Text(t) => format!(
            r#"<c r="{r}" s="{style}" t="inlineStr"><is><t xml:space="preserve">{}</t></is></c>"#,
            esc(t)
        ),
        Cell::Number(n) => format!(r#"<c r="{r}" s="{style}"><v>{n}</v></c>"#),
        Cell::Date(d) => format!(r#"<c r="{r}" s="{style}"><v>{}</v></c>"#, excel_serial(*d)),
    }
}

fn money(cents: Option<i64>) -> Cell {
    cents.map_or(Cell::Empty, |c| Cell::Number(c as f64 / 100.0))
}

fn date(d: Option<NaiveDate>) -> Cell {
    d.map_or(Cell::Empty, Cell::Date)
}

// ── Workbook parts ──────────────────────────────────────────────────────────

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

/// Two sheets: the register itself, named after the year to match the tab names in
/// Alex's workbook, and the monthly summary his Steuerberater reads.
fn workbook_xml(year: i32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>
<sheet name="{year}" sheetId="1" r:id="rId1"/>
<sheet name="Monatsübersicht" sheetId="2" r:id="rId3"/>
</sheets>
</workbook>"#
    )
}

/// Number formats and cell styles.
///
/// Custom `numFmtId`s must start at 164 — everything below is reserved by the spec.
fn styles_xml() -> String {
    r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="2">
<numFmt numFmtId="164" formatCode="DD\.MM\.YYYY"/>
<numFmt numFmtId="165" formatCode="#,##0.00\ &quot;&#8364;&quot;"/>
</numFmts>
<fonts count="3">
<font><sz val="11"/><name val="Calibri"/></font>
<font><b/><sz val="11"/><name val="Calibri"/></font>
<font><b/><sz val="14"/><name val="Calibri"/></font>
</fonts>
<fills count="3">
<fill><patternFill patternType="none"/></fill>
<fill><patternFill patternType="gray125"/></fill>
<fill><patternFill patternType="solid"><fgColor rgb="FFDDDDDD"/><bgColor indexed="64"/></patternFill></fill>
</fills>
<borders count="2">
<border><left/><right/><top/><bottom/><diagonal/></border>
<border><left/><right/><top style="thin"><color indexed="64"/></top><bottom/><diagonal/></border>
</borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="7">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="0" fontId="1" fillId="2" borderId="0" xfId="0" applyFont="1" applyFill="1"/>
<xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
<xf numFmtId="165" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
<xf numFmtId="0" fontId="2" fillId="0" borderId="0" xfId="0" applyFont="1"/>
<xf numFmtId="0" fontId="1" fillId="0" borderId="1" xfId="0" applyFont="1" applyBorder="1"/>
<xf numFmtId="165" fontId="1" fillId="0" borderId="1" xfId="0" applyNumberFormat="1" applyFont="1" applyBorder="1"/>
</cellXfs>
</styleSheet>"##
        .to_string()
}

/// Alex's column order, verbatim from the "2026" sheet of his workbook.
const HEADERS: [&str; 12] = [
    "Rg.-Nummer",
    "Datum",
    "Kunde",
    "Netto",
    "MWST",
    "Brutto",
    "Versendet",
    "Fällig",
    "Bezahlt am",
    "Offene Zahlungen",
    "Zahlungsart",
    "Bemerkungen",
];

/// Column widths, in Excel character units, sized to the longest value each column
/// actually holds in his book.
const WIDTHS: [f64; 12] = [12.0, 20.0, 32.0, 12.0, 12.0, 12.0, 12.0, 12.0, 12.0, 16.0, 12.0, 40.0];

fn sheet_xml(year: i32, rows: &[ExportRow]) -> String {
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

    // Rows 1-3: the two title lines and a blank spacer, exactly as his sheet opens.
    xml.push_str(&format!(
        r#"<row r="1">{}</row><row r="2">{}</row><row r="3"/>"#,
        cell_xml(0, 1, &Cell::Text(format!("Rechnungsausgangsbuch {year}")), style::TITLE),
        cell_xml(
            0,
            2,
            &Cell::Text("Aust Umzüge & Haushaltsauflösungen".to_string()),
            style::PLAIN
        ),
    ));

    // Row 4: headers.
    xml.push_str(r#"<row r="4">"#);
    for (c, h) in HEADERS.iter().enumerate() {
        xml.push_str(&cell_xml(c, 4, &Cell::Text((*h).to_string()), style::HEADER));
    }
    xml.push_str("</row>");

    // Rows 5..: the register itself, already in number order.
    let (mut sum_netto, mut sum_mwst, mut sum_brutto, mut sum_offen) = (0i64, 0i64, 0i64, 0i64);
    for (i, r) in rows.iter().enumerate() {
        let n = i + 5;
        sum_netto += r.netto_cents.unwrap_or(0);
        sum_mwst += r.mwst_cents.unwrap_or(0);
        sum_brutto += r.brutto_cents.unwrap_or(0);
        sum_offen += r.offen_cents.unwrap_or(0);

        // Alex's Offene-Zahlungen column carries the word "Bezahlt" once nothing is
        // outstanding, and the amount while something is. That word is what he scans
        // the column for, so the export keeps it rather than printing 0,00 €.
        let (offen_cell, offen_style) = match r.offen_cents {
            Some(0) => (Cell::Text("Bezahlt".to_string()), style::PLAIN),
            other => (money(other), style::MONEY),
        };

        let cells: [(Cell, u32); 12] = [
            (Cell::Text(r.invoice_number.clone()), style::PLAIN),
            (Cell::Text(r.service_period.clone()), style::PLAIN),
            (Cell::Text(r.customer.clone()), style::PLAIN),
            (money(r.netto_cents), style::MONEY),
            (money(r.mwst_cents), style::MONEY),
            (money(r.brutto_cents), style::MONEY),
            (date(r.sent_at), style::DATE),
            (date(r.due_date), style::DATE),
            (date(r.paid_at), style::DATE),
            (offen_cell, offen_style),
            (Cell::Text(r.payment_method.clone()), style::PLAIN),
            (Cell::Text(r.notes.clone()), style::PLAIN),
        ];

        xml.push_str(&format!(r#"<row r="{n}">"#));
        for (c, (cell, st)) in cells.iter().enumerate() {
            xml.push_str(&cell_xml(c, n, cell, *st));
        }
        xml.push_str("</row>");
    }

    // Totals row, one blank row below the last entry.
    let total_row = rows.len() + 6;
    xml.push_str(&format!(r#"<row r="{total_row}">"#));
    xml.push_str(&cell_xml(
        0,
        total_row,
        &Cell::Text(format!("Summe {year}")),
        style::TOTAL_TEXT,
    ));
    for c in 1..=2 {
        xml.push_str(&cell_xml(c, total_row, &Cell::Empty, style::TOTAL_TEXT));
    }
    for (c, v) in [sum_netto, sum_mwst, sum_brutto].iter().enumerate() {
        xml.push_str(&cell_xml(
            c + 3,
            total_row,
            &Cell::Number(*v as f64 / 100.0),
            style::TOTAL_MONEY,
        ));
    }
    for c in 6..=8 {
        xml.push_str(&cell_xml(c, total_row, &Cell::Empty, style::TOTAL_TEXT));
    }
    xml.push_str(&cell_xml(
        9,
        total_row,
        &Cell::Number(sum_offen as f64 / 100.0),
        style::TOTAL_MONEY,
    ));
    for c in 10..=11 {
        xml.push_str(&cell_xml(c, total_row, &Cell::Empty, style::TOTAL_TEXT));
    }
    xml.push_str("</row></sheetData></worksheet>");
    xml
}

/// German month names, index 0 = Januar.
const MONTH_NAMES: [&str; 12] = [
    "Januar", "Februar", "März", "April", "Mai", "Juni",
    "Juli", "August", "September", "Oktober", "November", "Dezember",
];

/// The monthly summary sheet.
///
/// Twelve rows, always — a month with no invoices is a zero row, not a missing one,
/// so the reader can see that February was quiet rather than wonder where it went.
///
/// Rows are bucketed by Rechnungsdatum (`sent_at`), because the Umsatzsteuer follows
/// the invoice date under Soll-Versteuerung. A row whose Rechnungsdatum falls outside
/// `year` — or which has none at all — is collected in a trailing "ohne Monat" row
/// rather than silently dropped: the sheet's total must reconcile with the register's.
fn summary_sheet_xml(year: i32, rows: &[ExportRow]) -> String {
    let mut buckets = [(0i64, 0i64, 0i64, 0i64, 0i64); 12]; // count, netto, mwst, brutto, offen
    let mut unassigned = (0i64, 0i64, 0i64, 0i64, 0i64);

    for r in rows {
        let slot = match r.sent_at {
            Some(d) if d.year() == year => Some((d.month() - 1) as usize),
            _ => None,
        };
        let b = match slot {
            Some(i) => &mut buckets[i],
            None => &mut unassigned,
        };
        b.0 += 1;
        b.1 += r.netto_cents.unwrap_or(0);
        b.2 += r.mwst_cents.unwrap_or(0);
        b.3 += r.brutto_cents.unwrap_or(0);
        b.4 += r.offen_cents.unwrap_or(0);
    }

    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols>"#,
    );
    for (i, w) in [16.0, 10.0, 14.0, 14.0, 14.0, 14.0].iter().enumerate() {
        xml.push_str(&format!(
            r#"<col min="{n}" max="{n}" width="{w}" customWidth="1"/>"#,
            n = i + 1
        ));
    }
    xml.push_str("</cols><sheetData>");

    xml.push_str(&format!(
        r#"<row r="1">{}</row><row r="2">{}</row><row r="3"/>"#,
        cell_xml(0, 1, &Cell::Text(format!("Monatsübersicht {year}")), style::TITLE),
        cell_xml(
            0,
            2,
            &Cell::Text("Umsatz nach Rechnungsdatum — Grundlage der Umsatzsteuer-Voranmeldung".to_string()),
            style::PLAIN
        ),
    ));

    let headers = ["Monat", "Rechnungen", "Netto", "MWST", "Brutto", "Offen"];
    xml.push_str(r#"<row r="4">"#);
    for (c, h) in headers.iter().enumerate() {
        xml.push_str(&cell_xml(c, 4, &Cell::Text((*h).to_string()), style::HEADER));
    }
    xml.push_str("</row>");

    let mut write_row = |xml: &mut String, n: usize, label: &str, b: &(i64, i64, i64, i64, i64)| {
        xml.push_str(&format!(r#"<row r="{n}">"#));
        xml.push_str(&cell_xml(0, n, &Cell::Text(label.to_string()), style::PLAIN));
        xml.push_str(&cell_xml(1, n, &Cell::Number(b.0 as f64), style::PLAIN));
        for (c, v) in [b.1, b.2, b.3, b.4].iter().enumerate() {
            xml.push_str(&cell_xml(c + 2, n, &Cell::Number(*v as f64 / 100.0), style::MONEY));
        }
        xml.push_str("</row>");
    };

    for (i, b) in buckets.iter().enumerate() {
        write_row(&mut xml, i + 5, MONTH_NAMES[i], b);
    }

    let mut next = 17;
    if unassigned.0 > 0 {
        write_row(&mut xml, next, "ohne Monat", &unassigned);
        next += 1;
    }

    let total = buckets.iter().chain(std::iter::once(&unassigned)).fold(
        (0i64, 0i64, 0i64, 0i64, 0i64),
        |acc, b| (acc.0 + b.0, acc.1 + b.1, acc.2 + b.2, acc.3 + b.3, acc.4 + b.4),
    );
    xml.push_str(&format!(r#"<row r="{next}">"#));
    xml.push_str(&cell_xml(0, next, &Cell::Text(format!("Summe {year}")), style::TOTAL_TEXT));
    xml.push_str(&cell_xml(1, next, &Cell::Number(total.0 as f64), style::TOTAL_TEXT));
    for (c, v) in [total.1, total.2, total.3, total.4].iter().enumerate() {
        xml.push_str(&cell_xml(c + 2, next, &Cell::Number(*v as f64 / 100.0), style::TOTAL_MONEY));
    }
    xml.push_str("</row></sheetData></worksheet>");
    xml
}

/// Build the .xlsx for one year of the register.
///
/// `rows` must already be in register (number) order — this writes them out as given.
pub(crate) fn build_xlsx(year: i32, rows: &[ExportRow]) -> Result<Vec<u8>, ApiError> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::<u8>::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let parts: [(&str, String); 7] = [
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", ROOT_RELS.to_string()),
        ("xl/workbook.xml", workbook_xml(year)),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS.to_string()),
        ("xl/styles.xml", styles_xml()),
        ("xl/worksheets/sheet1.xml", sheet_xml(year, rows)),
        ("xl/worksheets/sheet2.xml", summary_sheet_xml(year, rows)),
    ];

    for (name, body) in parts {
        zip.start_file(name, opts)
            .map_err(|e| ApiError::Internal(format!("XLSX-Export fehlgeschlagen: {e}")))?;
        zip.write_all(body.as_bytes())
            .map_err(|e| ApiError::Internal(format!("XLSX-Export fehlgeschlagen: {e}")))?;
    }

    let cursor = zip
        .finish()
        .map_err(|e| ApiError::Internal(format!("XLSX-Export fehlgeschlagen: {e}")))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
    }

    #[test]
    fn service_period_collapses_a_span_within_one_month() {
        assert_eq!(
            format_service_period(Some(d(2026, 1, 12)), Some(d(2026, 1, 13))),
            "12.-13.01.2026"
        );
    }

    #[test]
    fn service_period_spells_out_a_span_across_months() {
        assert_eq!(
            format_service_period(Some(d(2025, 11, 7)), Some(d(2026, 1, 30))),
            "07.11.2025-30.01.2026"
        );
    }

    #[test]
    fn service_period_of_a_single_day_is_that_day() {
        assert_eq!(format_service_period(Some(d(2026, 2, 24)), Some(d(2026, 2, 24))), "24.02.2026");
        assert_eq!(format_service_period(Some(d(2026, 2, 24)), None), "24.02.2026");
        assert_eq!(format_service_period(None, Some(d(2026, 2, 24))), "");
    }

    /// Bad data must not print a backwards range into a document the tax office reads.
    #[test]
    fn service_period_ignores_an_end_before_the_start() {
        assert_eq!(format_service_period(Some(d(2026, 5, 2)), Some(d(2026, 4, 1))), "02.05.2026");
    }

    /// The 1899-12-30 epoch is what makes a date land on the day Excel shows, given
    /// Excel's phantom 1900-02-29. Checked against the values Excel itself displays.
    /// (Only dates from 1900-03-01 onwards agree — the two months before the phantom
    /// day are off by one in every implementation, and no invoice is dated there.)
    #[test]
    fn excel_serial_matches_excels_own_epoch() {
        assert_eq!(excel_serial(d(1900, 3, 1)), 61);
        assert_eq!(excel_serial(d(2026, 1, 12)), 46034);
        assert_eq!(excel_serial(d(2026, 8, 21)), 46255);
    }

    #[test]
    fn cell_refs_carry_past_z() {
        assert_eq!(cell_ref(0, 1), "A1");
        assert_eq!(cell_ref(11, 4), "L4");
        assert_eq!(cell_ref(25, 2), "Z2");
        assert_eq!(cell_ref(26, 2), "AA2");
    }

    /// A customer name with an ampersand used to be enough to produce a file Excel
    /// refuses to open.
    #[test]
    fn escapes_xml_metacharacters_and_drops_control_bytes() {
        assert_eq!(esc("erfi & Co <KG>"), "erfi &amp; Co &lt;KG&gt;");
        assert_eq!(esc("a\u{0}b"), "ab");
        assert_eq!(esc("line\nbreak"), "line\nbreak");
    }

    fn sample_rows() -> Vec<ExportRow> {
        vec![
            ExportRow {
                invoice_number: "2026-01".into(),
                service_period: "12.-13.01.2026".into(),
                customer: "Luttert Ordnungs u. Regal Systeme & Co".into(),
                netto_cents: Some(148_800),
                mwst_cents: Some(28_272),
                brutto_cents: Some(177_072),
                sent_at: Some(d(2026, 1, 20)),
                due_date: Some(d(2026, 1, 30)),
                paid_at: Some(d(2026, 1, 27)),
                offen_cents: Some(0),
                payment_method: "EC".into(),
                notes: String::new(),
            },
            ExportRow {
                invoice_number: "2026-02".into(),
                service_period: "27.07.2026".into(),
                customer: "Gabriele Kampe".into(),
                netto_cents: Some(107_000),
                mwst_cents: Some(20_330),
                brutto_cents: Some(127_330),
                sent_at: Some(d(2026, 8, 3)),
                due_date: None,
                paid_at: None,
                offen_cents: Some(127_330),
                payment_method: "EC".into(),
                notes: "stand 20.08.26 keine überweisung".into(),
            },
        ]
    }

    #[test]
    fn builds_a_readable_zip_with_every_required_part() {
        let bytes = build_xlsx(2026, &sample_rows()).expect("export");
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("valid zip");
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).expect("entry").name().to_string())
            .collect();
        for required in [
            "[Content_Types].xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "xl/styles.xml",
            "xl/worksheets/sheet1.xml",
        ] {
            assert!(names.contains(&required.to_string()), "missing {required} in {names:?}");
        }
    }

    #[test]
    fn sheet_carries_alex_column_order_and_the_bezahlt_marker() {
        let xml = sheet_xml(2026, &sample_rows());
        for h in HEADERS {
            assert!(xml.contains(&esc(h)), "header {h} missing");
        }
        // Settled row prints the word, open row prints the amount.
        assert!(xml.contains("Bezahlt"), "settled row must say Bezahlt, not 0,00 €");
        assert!(xml.contains("<v>1273.3</v>"), "open row must carry its amount");
        assert!(xml.contains("Luttert Ordnungs u. Regal Systeme &amp; Co"));
    }

    /// Totals must sum the rows, and must land below the last entry rather than on it.
    #[test]
    fn totals_row_sums_the_year() {
        let xml = sheet_xml(2026, &sample_rows());
        assert!(xml.contains(r#"<row r="8">"#), "totals row expected at 5 + 2 rows + 1 blank");
        assert!(xml.contains("Summe 2026"));
        assert!(xml.contains("<v>2558</v>"), "netto 1488.00 + 1070.00");
        assert!(xml.contains("<v>3044.02</v>"), "brutto 1770.72 + 1273.30");
    }

    #[test]
    fn summary_sheet_buckets_by_month_and_reconciles_with_the_register() {
        let xml = summary_sheet_xml(2026, &sample_rows());
        for name in MONTH_NAMES {
            assert!(xml.contains(name), "month {name} missing from the summary");
        }
        // Row 1 is January-dated (20.01.), row 2 August-dated (03.08.).
        assert!(xml.contains("<v>1488</v>"), "January netto");
        assert!(xml.contains("<v>1070</v>"), "August netto");
        // The year total must equal the register's own sum, or the two sheets disagree.
        assert!(xml.contains("<v>2558</v>"), "netto total 1488.00 + 1070.00");
        assert!(xml.contains("Summe 2026"));
    }

    /// A row whose Rechnungsdatum falls outside the year — or which has none — must be
    /// collected, not dropped, or the summary stops reconciling with the register.
    #[test]
    fn summary_sheet_collects_rows_with_no_month_of_this_year() {
        let mut rows = sample_rows();
        rows.push(ExportRow {
            invoice_number: "2026-03".into(),
            customer: "Praxis Günter Engelhardt".into(),
            netto_cents: Some(12_000),
            brutto_cents: Some(14_280),
            // Issued in the previous December, but numbered into the 2026 book.
            sent_at: Some(d(2025, 12, 23)),
            ..Default::default()
        });
        let xml = summary_sheet_xml(2026, &rows);
        assert!(xml.contains("ohne Monat"), "an out-of-year row needs its own line");
        assert!(xml.contains("<v>2678</v>"), "total must include it: 1488 + 1070 + 120");
    }

    #[test]
    fn summary_sheet_keeps_quiet_months_as_zero_rows() {
        let xml = summary_sheet_xml(2026, &[]);
        // All twelve, so a quiet February reads as quiet rather than as missing.
        for name in MONTH_NAMES {
            assert!(xml.contains(name), "month {name} dropped from an empty year");
        }
    }

    #[test]
    fn workbook_declares_both_sheets() {
        let bytes = build_xlsx(2026, &sample_rows()).expect("export");
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("valid zip");
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).expect("entry").name().to_string())
            .collect();
        assert!(names.contains(&"xl/worksheets/sheet2.xml".to_string()), "{names:?}");
    }

    #[test]
    fn an_empty_year_still_produces_a_valid_file() {
        let bytes = build_xlsx(2027, &[]).expect("export");
        assert!(zip::ZipArchive::new(Cursor::new(bytes)).is_ok());
    }
}
