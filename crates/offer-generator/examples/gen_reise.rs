use std::fs;

fn main() {
    let data = aust_offer_generator::TravelExpenseData {
        employee_first_name: "Max".into(),
        employee_last_name: "Mustermann".into(),
        start_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 23).unwrap(),
        start_time: chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
        end_date: chrono::NaiveDate::from_ymd_opt(2026, 3, 26).unwrap(),
        end_time: chrono::NaiveTime::from_hms_opt(16, 30, 0).unwrap(),
        destination: "Rüsselsheim".into(),
        reason: "Luttert Montage".into(),
        transport_mode: Some("PKW".into()),
        travel_costs_eur: 0.0,
        small_days: 2,
        large_days: 2,
        breakfast_deduction_eur: 0.0,
        meal_deduction_eur: 0.0,
        accommodation_eur: 0.0,
        misc_costs_eur: 0.0,
    };
    let bytes = aust_offer_generator::generate_travel_expense_xlsx(&data).expect("should generate");
    fs::write("/tmp/reisekosten_employee_name_test.xlsx", bytes).expect("write");
    println!("Written to /tmp/reisekosten_employee_name_test.xlsx");
}
