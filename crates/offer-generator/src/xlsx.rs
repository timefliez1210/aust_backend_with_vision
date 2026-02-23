use crate::OfferError;
use serde::{Deserialize, Serialize};
use umya_spreadsheet::Spreadsheet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferData {
    pub offer_number: String,
    pub date: chrono::NaiveDate,
    pub customer_salutation: String,
    pub customer_name: String,
    pub customer_street: String,
    pub customer_city: String,
    pub customer_phone: String,
    pub customer_email: String,
    pub greeting: String,
    pub moving_date: String,
    pub origin_street: String,
    pub origin_city: String,
    pub origin_floor_info: String,
    pub dest_street: String,
    pub dest_city: String,
    pub dest_floor_info: String,
    pub volume_m3: f64,
    pub persons: u32,
    pub estimated_hours: f64,
    pub rate_per_person_hour: f64,
    pub line_items: Vec<OfferLineItem>,
    pub detected_items: Vec<DetectedItemRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferLineItem {
    pub row: u32,
    pub description: Option<String>,
    pub quantity: f64,
    pub unit_price: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedItemRow {
    pub name: String,
    pub volume_m3: f64,
    pub dimensions: Option<String>,
    pub confidence: f64,
    /// German name from RE catalog
    #[serde(default)]
    pub german_name: Option<String>,
    /// RE value (1 RE = 0.1 m³)
    #[serde(default)]
    pub re_value: Option<f64>,
    /// "re_lookup" or "geometric"
    #[serde(default)]
    pub volume_source: Option<String>,
    #[serde(default)]
    pub crop_s3_key: Option<String>,
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    #[serde(default)]
    pub bbox_image_index: Option<usize>,
    #[serde(default)]
    pub source_image_urls: Option<Vec<String>>,
}

pub struct XlsxGenerator {
    workbook: Spreadsheet,
}

impl XlsxGenerator {
    pub fn from_template(template_bytes: &[u8]) -> Result<Self, OfferError> {
        let cursor = std::io::Cursor::new(template_bytes);
        let workbook = umya_spreadsheet::reader::xlsx::read_reader(cursor, true)
            .map_err(|e| OfferError::Template(format!("Failed to read xlsx template: {e}")))?;
        Ok(Self { workbook })
    }

    pub fn generate(mut self, data: &OfferData) -> Result<Vec<u8>, OfferError> {
        self.fill_main_sheet(data)?;
        self.add_items_sheet(data)?;
        self.fix_print_area();
        self.write_bytes()
    }

    /// Remove columns I-P from the print area so the PDF only contains
    /// the customer-facing offer (columns A-H), not the internal calculation sheet.
    fn fix_print_area(&mut self) {
        for dn in self.workbook.get_defined_names_mut().iter_mut() {
            if dn.get_name() == "_xlnm.Print_Area" {
                // Replace multi-range print area with just the offer columns
                dn.set_address("'Tabelle1'!$A$1:$H$120");
                break;
            }
        }
    }

    fn fill_main_sheet(&mut self, data: &OfferData) -> Result<(), OfferError> {
        let sheet = self
            .workbook
            .get_sheet_mut(&0)
            .ok_or_else(|| OfferError::Template("Cannot access first sheet".into()))?;

        // Customer address block
        set_cell_text(sheet, "A", 8, &data.customer_salutation);
        set_cell_text(sheet, "A", 9, &data.customer_name);
        set_cell_text(sheet, "A", 10, &data.customer_street);
        set_cell_text(sheet, "A", 11, &data.customer_city);

        // Date — replace the TODAY() formula with actual date
        let date_str = data.date.format("%d.%m.%Y").to_string();
        let cell_g14 = sheet.get_cell_mut("G14");
        cell_g14.set_formula("");
        cell_g14.set_value_string(&date_str);

        // Title row
        set_cell_text(
            sheet,
            "A",
            16,
            &format!("Unverbindlicher Kostenvoranschlag {}", data.offer_number),
        );

        // Moving date & contact
        set_cell_text(sheet, "B", 17, &data.moving_date);
        set_cell_text(sheet, "B", 18, &data.customer_phone);
        set_cell_text(sheet, "F", 18, &data.customer_email);

        // Greeting
        set_cell_text(sheet, "A", 20, &data.greeting);

        // Origin address
        set_cell_text(sheet, "A", 26, &data.origin_street);
        set_cell_text(sheet, "A", 27, &data.origin_city);
        set_cell_text(sheet, "A", 28, &data.origin_floor_info);

        // Destination address
        set_cell_text(sheet, "F", 26, &data.dest_street);
        set_cell_text(sheet, "F", 27, &data.dest_city);
        set_cell_text(sheet, "F", 28, &data.dest_floor_info);

        // Volume description
        set_cell_text(
            sheet,
            "A",
            29,
            &format!("Umzugspauschale {:.1} m³", data.volume_m3),
        );

        // Clear all template preset quantities/prices in line item rows (31-42, except 38=labor).
        // This ensures only our explicit values contribute to the netto total (G44).
        for row in 31..=42 {
            if row == 38 {
                continue; // labor row handled separately below
            }
            set_cell_number(sheet, "E", row, 0.0);
            set_cell_number(sheet, "F", row, 0.0);
        }

        // Line items: fill E and F columns (quantity and unit price)
        // Formulas in G column (=IF(E="",0,F*E)) are preserved.
        for item in &data.line_items {
            set_cell_number(sheet, "E", item.row, item.quantity);
            set_cell_number(sheet, "F", item.row, item.unit_price);
            if let Some(desc) = &item.description {
                set_cell_text(sheet, "D", item.row, desc);
            }
        }

        // Labor line (row 38): "{N} Umzugshelfer", hours, rate
        set_cell_text(
            sheet,
            "D",
            38,
            &format!("{} Umzugshelfer", data.persons),
        );
        set_cell_number(sheet, "E", 38, data.estimated_hours);
        set_cell_number(sheet, "F", 38, data.rate_per_person_hour);

        // Number of persons in J50 (used by G38 formula)
        set_cell_number(sheet, "J", 50, data.persons as f64);

        Ok(())
    }

    fn add_items_sheet(&mut self, data: &OfferData) -> Result<(), OfferError> {
        if data.detected_items.is_empty() {
            return Ok(());
        }

        let sheet_name = "Erfasste Gegenstände";
        self.workbook.new_sheet(sheet_name)
            .map_err(|e| OfferError::Template(format!("Failed to create items sheet: {e}")))?;

        let sheet = self
            .workbook
            .get_sheet_by_name_mut(sheet_name)
            .ok_or_else(|| OfferError::Template("Items sheet not found after creation".into()))?;

        // Column widths
        sheet.get_column_dimension_mut("A").set_width(8.0);   // Nr.
        sheet.get_column_dimension_mut("B").set_width(32.0);  // Gegenstand
        sheet.get_column_dimension_mut("C").set_width(28.0);  // Bezeichnung (deutsch)
        sheet.get_column_dimension_mut("D").set_width(10.0);  // RE
        sheet.get_column_dimension_mut("E").set_width(14.0);  // Volumen (m³)
        sheet.get_column_dimension_mut("F").set_width(22.0);  // Maße
        sheet.get_column_dimension_mut("G").set_width(10.0);  // Quelle

        // Header row with bold + bottom border
        let headers = ["Nr.", "Gegenstand", "Bezeichnung (DE)", "RE", "Volumen (m³)", "Maße (L×B×H)", "Quelle"];
        let header_cols = ["A", "B", "C", "D", "E", "F", "G"];
        for (col_idx, header) in headers.iter().enumerate() {
            let col = header_cols[col_idx];
            let cell = sheet.get_cell_mut(format!("{col}1"));
            cell.set_value_string(*header);
            let style = cell.get_style_mut();
            style.get_font_mut().set_bold(true);
            // Bottom border
            style
                .get_borders_mut()
                .get_bottom_mut()
                .set_border_style(umya_spreadsheet::Border::BORDER_THIN);
        }

        // Light gray for alternating rows
        let light_gray = "F2F2F2";

        // Data rows
        let mut total_volume = 0.0;
        for (i, item) in data.detected_items.iter().enumerate() {
            let row = (i + 2) as u32;
            let is_even = i % 2 == 0;

            // Nr.
            set_cell_number(sheet, "A", row, (i + 1) as f64);

            // Gegenstand (detected name)
            set_cell_text(sheet, "B", row, &item.name);

            // Bezeichnung (German name from RE catalog)
            if let Some(ref german) = item.german_name {
                set_cell_text(sheet, "C", row, german);
            }

            // RE value
            if let Some(re) = item.re_value {
                set_cell_number(sheet, "D", row, re);
            }

            // Volumen — formatted to 2 decimal places
            set_cell_number(sheet, "E", row, item.volume_m3);
            sheet
                .get_cell_mut(format!("E{row}"))
                .get_style_mut()
                .get_number_format_mut()
                .set_format_code("0.00");

            // Maße
            if let Some(dims) = &item.dimensions {
                set_cell_text(sheet, "F", row, dims);
            }

            // Quelle
            if let Some(ref src) = item.volume_source {
                let label = match src.as_str() {
                    "re_lookup" => "RE-Katalog",
                    "geometric" => "3D-Messung",
                    _ => src.as_str(),
                };
                set_cell_text(sheet, "G", row, label);
            }

            // Alternating row shading
            if is_even {
                for col in &header_cols {
                    sheet
                        .get_cell_mut(format!("{col}{row}"))
                        .get_style_mut()
                        .set_background_color(light_gray);
                }
            }

            total_volume += item.volume_m3;
        }

        // Total row with top border
        let total_row = (data.detected_items.len() + 3) as u32; // skip a blank row
        let total_cell = sheet.get_cell_mut(format!("B{total_row}"));
        total_cell.set_value_string("Gesamtvolumen");
        total_cell.get_style_mut().get_font_mut().set_bold(true);
        total_cell
            .get_style_mut()
            .get_borders_mut()
            .get_top_mut()
            .set_border_style(umya_spreadsheet::Border::BORDER_THIN);

        set_cell_number(sheet, "E", total_row, total_volume);
        let vol_cell = sheet.get_cell_mut(format!("E{total_row}"));
        vol_cell.get_style_mut().get_font_mut().set_bold(true);
        vol_cell
            .get_style_mut()
            .get_number_format_mut()
            .set_format_code("0.00");
        vol_cell
            .get_style_mut()
            .get_borders_mut()
            .get_top_mut()
            .set_border_style(umya_spreadsheet::Border::BORDER_THIN);

        // Item count
        let count_row = total_row + 1;
        set_cell_text(sheet, "B", count_row, "Anzahl Gegenstände");
        set_cell_number(sheet, "E", count_row, data.detected_items.len() as f64);

        Ok(())
    }

    fn write_bytes(self) -> Result<Vec<u8>, OfferError> {
        let mut buf = std::io::Cursor::new(Vec::new());
        umya_spreadsheet::writer::xlsx::write_writer(&self.workbook, &mut buf)
            .map_err(|e| OfferError::Template(format!("Failed to write xlsx: {e}")))?;
        Ok(buf.into_inner())
    }
}

fn set_cell_text(
    sheet: &mut umya_spreadsheet::Worksheet,
    col: &str,
    row: u32,
    value: &str,
) {
    sheet
        .get_cell_mut(format!("{col}{row}"))
        .set_value_string(value);
}

fn set_cell_number(
    sheet: &mut umya_spreadsheet::Worksheet,
    col: &str,
    row: u32,
    value: f64,
) {
    sheet
        .get_cell_mut(format!("{col}{row}"))
        .set_value_number(value);
}
