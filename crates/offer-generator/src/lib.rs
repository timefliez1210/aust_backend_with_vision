pub mod error;

mod pdf_convert;
mod pricing;
mod xlsx;

pub use error::OfferError;
pub use pdf_convert::convert_xlsx_to_pdf;
pub use pricing::{parse_floor, PricingEngine};
pub use xlsx::{generate_offer_xlsx, DetectedItemRow, OfferData, OfferLineItem};
