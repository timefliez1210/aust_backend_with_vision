//! Shared ZIP/XML plumbing for the XLSX template generators.
//!
//! `xlsx.rs` (offers), `invoice_xlsx.rs` (invoices), and `travel_expense_xlsx.rs`
//! (travel expenses) all do string-based XML surgery on a template `.xlsx` (ZIP)
//! file: read an entry, mutate the XML as a string, then rebuild the ZIP with
//! the mutated entries swapped in. This module holds the parts of that dance
//! that are identical across all three generators.

use crate::OfferError;
use std::io::{Cursor, Read, Write};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// Read a named entry from a `ZipArchive` into a `Vec<u8>`.
///
/// **Why**: Every XML file in a template ZIP must be extracted before it can be
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
pub(crate) fn read_zip_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Vec<u8>, OfferError> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| OfferError::Template(format!("ZIP entry '{name}' not found: {e}")))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| OfferError::Template(format!("Failed to read ZIP entry '{name}': {e}")))?;
    Ok(data)
}

/// Remove the `<hyperlinks>…</hyperlinks>` section from a worksheet XML string.
///
/// **Why**: Templates are saved with email addresses formatted as clickable
/// hyperlinks. When LibreOffice converts the file to PDF it renders those as
/// blue underlined links. The generated document should display the address
/// as plain text instead. Removing the `<hyperlinks>` block demotes those
/// cells to plain inline strings.
///
/// The companion function `strip_hyperlink_rels` removes the corresponding
/// relationship entries from the sheet's `.rels` file.
///
/// # Parameters
/// - `xml` — raw worksheet XML content (e.g. `sheet1.xml`)
///
/// # Returns
/// Modified string with the entire `<hyperlinks>…</hyperlinks>` block removed.
/// Returns the original if the block is not found.
pub(crate) fn strip_hyperlinks(xml: &str) -> String {
    if let Some(start) = xml.find("<hyperlinks>")
        && let Some(end_tag) = xml[start..].find("</hyperlinks>") {
            let abs_end = start + end_tag + "</hyperlinks>".len();
            let mut result = String::with_capacity(xml.len());
            result.push_str(&xml[..start]);
            result.push_str(&xml[abs_end..]);
            return result;
        }
    xml.to_string()
}

/// Remove all hyperlink `<Relationship>` entries from a worksheet `.rels` file.
///
/// **Why**: Each hyperlink in the sheet has a corresponding relationship entry of
/// type `…/hyperlink`. After removing the `<hyperlinks>` block from the sheet
/// XML, these relationship entries are orphaned and can cause validation
/// warnings. Non-hyperlink relationships (e.g. the drawing relationship for a
/// logo image, or a table relationship) are preserved.
///
/// Scans every `<Relationship .../>` element in the file and drops the ones
/// whose fragment contains `"hyperlink"`, regardless of where in the file they
/// appear.
///
/// # Parameters
/// - `xml` — raw `.rels` content, e.g. `xl/worksheets/_rels/sheet1.xml.rels`
///
/// # Returns
/// Modified string with all hyperlink `<Relationship>` elements removed.
pub(crate) fn strip_hyperlink_rels(xml: &str) -> String {
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
            // Not a hyperlink — keep it, and advance past the tag name so the
            // next search doesn't just find this same element again.
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

/// Map a `zip::result::ZipError` to `OfferError::Template`.
pub(crate) fn map_zip(e: zip::result::ZipError) -> OfferError {
    OfferError::Template(format!("ZIP error: {e}"))
}

/// Map a `std::io::Error` to `OfferError::Template`.
pub(crate) fn map_io(e: std::io::Error) -> OfferError {
    OfferError::Template(format!("IO error: {e}"))
}

/// Copy every entry from `template_zip` into `writer`, substituting the content
/// of any entry named in `replacements` and dropping any entry for which `skip`
/// returns `true`. All other entries are copied through unchanged.
///
/// **Why**: All three generators reassemble their output ZIP the same way —
/// walk every entry of the template archive, swap in the handful of XML parts
/// that were modified, and copy the rest bit-for-bit. This factors out that
/// walk; callers differ only in which entries they replace/skip and whether
/// they append extra entries afterwards, so `writer` is left open for the
/// caller to finish assembling (append extra files, then call `.finish()`).
///
/// # Parameters
/// - `template_zip` — the already-opened template `ZipArchive`
/// - `writer` — the in-progress output `ZipWriter`
/// - `options` — file write options (compression method etc.) to use for every entry
/// - `replacements` — `(entry_name, new_content)` pairs; matching entries get this content instead of the template's
/// - `skip` — entries for which this returns `true` are omitted from the output entirely
///
/// # Errors
/// - `OfferError::Template` if any template ZIP entry cannot be read
/// - `OfferError::Template` if writing to the output ZIP fails
pub(crate) fn copy_zip_entries<W: Write + std::io::Seek>(
    template_zip: &mut ZipArchive<Cursor<&[u8]>>,
    writer: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    replacements: &[(&str, &str)],
    skip: impl Fn(&str) -> bool,
) -> Result<(), OfferError> {
    for i in 0..template_zip.len() {
        let mut file = template_zip.by_index(i).map_err(|e| {
            OfferError::Template(format!("Failed to read template entry {i}: {e}"))
        })?;
        let name = file.name().to_string();

        if skip(&name) {
            continue;
        }

        if let Some((_, content)) = replacements.iter().find(|(n, _)| *n == name) {
            writer.start_file(&name, options).map_err(map_zip)?;
            writer.write_all(content.as_bytes()).map_err(map_io)?;
        } else {
            let mut data = Vec::new();
            file.read_to_end(&mut data).map_err(|e| {
                OfferError::Template(format!("Failed to read template entry {name}: {e}"))
            })?;
            writer.start_file(&name, options).map_err(map_zip)?;
            writer.write_all(&data).map_err(map_io)?;
        }
    }
    Ok(())
}
