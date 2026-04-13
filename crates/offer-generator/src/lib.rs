pub mod error;

mod invoice_xlsx;
mod pdf_convert;
mod pricing;
mod timesheet_xlsx;
mod xlsx;

pub use error::OfferError;
pub use invoice_xlsx::{generate_invoice_xlsx, ExtraService, InvoiceData, InvoiceLineItem, InvoiceType};
pub use pdf_convert::convert_xlsx_to_pdf;
pub use pricing::{parse_floor, PricingEngine};
pub use timesheet_xlsx::{generate_timesheet_xlsx, TimesheetData, TimesheetEntry};
pub use xlsx::{
    generate_offer_xlsx, DetectedItemRow, OfferData, OfferLineItem,
    hide_row, unhide_row, set_cell_value as offer_set_cell_value,
    strip_formula_cached_values, format_number, xml_escape, CellValue as OfferCellValue,
};
