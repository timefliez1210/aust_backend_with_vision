//! Invoice number format — parse, format, and ordering.
//!
//! An invoice number is `YYYY-N`: the calendar year the invoice belongs to, a
//! hyphen, and that year's running counter. The counter restarts at 1 every
//! January, exactly like the numbers in Alex's paper/Excel Rechnungsausgangsbuch
//! ("2026-01" … "2026-86", then "2027-01").
//!
//! # Zero padding
//! Numbers issued before 2026-08-21 were padded to four digits (`2026-0087`)
//! because the allocator was a single global sequence that never reset. Those
//! numbers are already on invoices in customers' hands and are left untouched;
//! only newly issued numbers use the unpadded form. Everything in this module
//! therefore parses padded and unpadded numbers identically — `2026-0087` and
//! `2026-87` are the same (year, seq) pair — so a register that mixes both
//! still sorts and counts correctly.

/// Split an invoice number into `(year, sequence)`.
///
/// Returns `None` for anything that is not `YYYY-N` with both parts numeric —
/// test fixtures (`TEST-<uuid>`) and any free-form number Alex typed by hand.
/// Callers must treat `None` as "unorderable / unknown year", never as an error.
pub fn parse(invoice_number: &str) -> Option<(i32, i64)> {
    let (year, seq) = invoice_number.trim().split_once('-')?;
    if year.len() != 4 {
        return None;
    }
    Some((year.parse::<i32>().ok()?, seq.parse::<i64>().ok()?))
}

/// Render `(year, sequence)` as the number Alex reads in his book.
///
/// Two-digit minimum so a year's first nine invoices line up in a column
/// (`2026-01` … `2026-09`, `2026-10`), which is how his sheet is written.
pub fn format(year: i32, seq: i64) -> String {
    format!("{year}-{seq:02}")
}

/// Sort key placing a register in true number order.
///
/// The register is a running ledger and is read as a number sequence, so this is
/// the only correct order for it. Sorting by date is not equivalent: Alex's
/// invoice dates are not monotonic (2026-25 is dated 29.05., 2026-26 13.03.),
/// so a date sort scrambles the sequence.
///
/// Unparseable numbers sort last, ordered by their raw text, rather than being
/// dropped or crashing the comparison.
pub fn sort_key(invoice_number: &str) -> (i32, i64, String) {
    match parse(invoice_number) {
        Some((year, seq)) => (year, seq, String::new()),
        None => (i32::MAX, i64::MAX, invoice_number.to_string()),
    }
}

/// The calendar year a register row is booked under.
///
/// The number prefix wins over any date on the row: Alex issues invoices in
/// January for work done the previous December (2026-02 carries a Leistungsdatum
/// of 23.12.2025) and they belong in the 2026 book, because that is the book
/// their number came from. `fallback_year` covers numbers this module cannot
/// parse and should be the row's invoice/creation date year.
pub fn register_year(invoice_number: &str, fallback_year: i32) -> i32 {
    parse(invoice_number).map_or(fallback_year, |(year, _)| year)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_padded_and_unpadded_identically() {
        assert_eq!(parse("2026-0087"), Some((2026, 87)));
        assert_eq!(parse("2026-87"), Some((2026, 87)));
        assert_eq!(parse("  2026-87 "), Some((2026, 87)));
    }

    #[test]
    fn rejects_free_form_numbers() {
        assert_eq!(parse("TEST-abc"), None);
        assert_eq!(parse("2026"), None);
        assert_eq!(parse("26-1"), None);
        assert_eq!(parse("2026-"), None);
        assert_eq!(parse("2026-1a"), None);
    }

    #[test]
    fn formats_with_two_digit_minimum() {
        assert_eq!(format(2026, 1), "2026-01");
        assert_eq!(format(2026, 9), "2026-09");
        assert_eq!(format(2026, 87), "2026-87");
        assert_eq!(format(2026, 148), "2026-148");
    }

    /// The register must read as 1, 2, … 10, 11 — not 1, 10, 11, 2.
    #[test]
    fn sorts_numerically_not_lexically() {
        let mut nums = vec!["2026-11", "2026-2", "2026-0001", "2025-99", "2026-10"];
        nums.sort_by_key(|n| sort_key(n));
        assert_eq!(nums, vec!["2025-99", "2026-0001", "2026-2", "2026-10", "2026-11"]);
    }

    #[test]
    fn unparseable_numbers_sort_last_without_being_dropped() {
        let mut nums = vec!["TEST-zz", "2027-01", "TEST-aa", "2026-99"];
        nums.sort_by_key(|n| sort_key(n));
        assert_eq!(nums, vec!["2026-99", "2027-01", "TEST-aa", "TEST-zz"]);
    }

    /// A December job invoiced in January belongs to January's book.
    #[test]
    fn register_year_follows_the_number_not_the_date() {
        assert_eq!(register_year("2026-02", 2025), 2026);
        assert_eq!(register_year("TEST-x", 2025), 2025);
    }
}
