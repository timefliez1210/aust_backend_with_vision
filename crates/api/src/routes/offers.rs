//! Offer route helpers — thin re-export layer.
//!
//! All pipeline logic lives in `crate::services::offer_builder`. This module
//! re-exports the public surface so existing callers (`orchestrator`, `inquiry_actions`,
//! `repositories/customer_repo`, `services/inquiry_builder`, etc.) continue to resolve
//! their imports from `crate::routes::offers` without changes.

pub(crate) use crate::services::offer_builder::{
    build_offer,
    build_offer_with_overrides,
    detect_salutation_and_greeting,
    parse_detected_items,
    GeneratedOffer,
    OfferOverrides,
    VolumeEstimationRow,
};
