//! Offer route helpers — thin re-export layer.
//!
//! All pipeline logic lives in `crate::services::offer_builder`. This module
//! re-exports the public surface so route-level callers (`inquiry_actions`, etc.)
//! can import from `crate::routes::offers` without reaching into the service layer.

pub(crate) use crate::services::offer_builder::{build_offer_with_overrides, OfferOverrides};
