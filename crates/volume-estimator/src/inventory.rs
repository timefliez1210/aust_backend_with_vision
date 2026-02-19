use crate::VolumeError;
use aust_core::models::{InventoryForm, InventoryItem};

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

    pub fn parse_csv(&self, csv_data: &str) -> Result<Vec<InventoryItem>, VolumeError> {
        let mut items = Vec::new();

        for line in csv_data.lines().skip(1) {
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() >= 3 {
                let item = InventoryItem {
                    name: parts[0].trim().to_string(),
                    quantity: parts[1]
                        .trim()
                        .parse()
                        .map_err(|_| VolumeError::InvalidData("Invalid quantity".into()))?,
                    volume_m3: parts[2]
                        .trim()
                        .parse()
                        .map_err(|_| VolumeError::InvalidData("Invalid volume".into()))?,
                    category: parts.get(3).map(|s| s.trim().to_string()),
                };
                items.push(item);
            }
        }

        Ok(items)
    }
}

impl Default for InventoryProcessor {
    fn default() -> Self {
        Self::new()
    }
}
