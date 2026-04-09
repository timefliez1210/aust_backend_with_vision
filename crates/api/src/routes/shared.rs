//! Shared route utilities.
//!
//! `InquiryRow` has moved to `crate::types`. It is kept here as a re-export so
//! route-level test code and any future route callers do not need to reach
//! directly into `crate::types`.

#[cfg(test)]
mod tests {
    use crate::types::InquiryRow;
    use aust_core::models::{Inquiry, InquiryStatus};
    use uuid::Uuid;

    fn make_test_inquiry_row(status: &str) -> InquiryRow {
        let dummy_id = Uuid::nil();
        let now = chrono::DateTime::from_timestamp(0, 0).unwrap();
        InquiryRow {
            id: dummy_id,
            customer_id: dummy_id,
            origin_address_id: None,
            destination_address_id: None,
            stop_address_id: None,
            status: status.to_string(),
            estimated_volume_m3: None,
            distance_km: None,
            scheduled_date: None,
            notes: None,
            services: serde_json::json!({}),
            source: "direct_email".to_string(),
            offer_sent_at: None,
            accepted_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn inquiry_row_status_mapping() {
        let cases = [
            ("pending", InquiryStatus::Pending),
            ("info_requested", InquiryStatus::InfoRequested),
            ("estimating", InquiryStatus::Estimating),
            ("estimated", InquiryStatus::Estimated),
            ("offer_ready", InquiryStatus::OfferReady),
            ("offer_sent", InquiryStatus::OfferSent),
            ("accepted", InquiryStatus::Accepted),
            ("rejected", InquiryStatus::Rejected),
            ("expired", InquiryStatus::Expired),
            ("cancelled", InquiryStatus::Cancelled),
            ("scheduled", InquiryStatus::Scheduled),
            ("completed", InquiryStatus::Completed),
            ("invoiced", InquiryStatus::Invoiced),
            ("paid", InquiryStatus::Paid),
            ("unknown_value", InquiryStatus::Pending),
        ];

        for (status_str, expected) in cases {
            let row = make_test_inquiry_row(status_str);
            let inquiry = Inquiry::from(row);
            assert_eq!(
                inquiry.status, expected,
                "status '{}' should map to {:?}",
                status_str, expected
            );
        }
    }
}
