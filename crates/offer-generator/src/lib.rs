pub mod error;

mod pdf;
mod pdf_convert;
mod pricing;
mod templates;
mod xlsx;

pub use error::OfferError;
pub use pdf::PdfGenerator;
pub use pdf_convert::convert_xlsx_to_pdf;
pub use pricing::{parse_floor, PricingEngine};
pub use templates::OfferTemplate;
pub use xlsx::{generate_offer_xlsx, DetectedItemRow, OfferData, OfferLineItem};
