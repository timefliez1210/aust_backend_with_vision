use crate::VolumeError;
use aust_core::models::InventoryForm;

pub struct InventoryProcessor;

impl InventoryProcessor {
    pub fn new() -> Self {
        Self
    }

    pub fn process_form(&self, form: &InventoryForm) -> Result<f64, VolumeError> {
        let total: f64 = form
            .items
            .iter()
            .map(|item| item.volume_m3 * item.quantity as f64)
            .sum();

        Ok(total)
    }
}

impl Default for InventoryProcessor {
    fn default() -> Self {
        Self::new()
    }
}
