/// Test route calculation with real OpenRouteService API.
/// Run with: cargo run -p aust-distance-calculator --example test_route
use aust_distance_calculator::{RouteCalculator, RouteRequest};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .init();

    let api_key = std::env::var("AUST__MAPS__API_KEY")
        .expect("Set AUST__MAPS__API_KEY env var");

    let calculator = RouteCalculator::new(api_key);

    // Test 1: Two addresses (simple A→B)
    println!("=== Test 1: Two addresses ===\n");
    let request = RouteRequest {
        addresses: vec![
            "Kaiserstr. 32, 31134 Hildesheim".to_string(),
            "Bahnhofstr. 1, 30159 Hannover".to_string(),
        ],
    };

    match calculator.calculate(&request).await {
        Ok(result) => print_result(&result),
        Err(e) => eprintln!("FAILED: {e}"),
    }

    // Test 2: Three addresses (A→B→C with intermediate stop)
    println!("\n=== Test 2: Three addresses (with Zwischenstopp) ===\n");
    let request = RouteRequest {
        addresses: vec![
            "Kaiserstr. 32, 31134 Hildesheim".to_string(),
            "Marktplatz 1, 38100 Braunschweig".to_string(),
            "Kröpcke 1, 30159 Hannover".to_string(),
        ],
    };

    match calculator.calculate(&request).await {
        Ok(result) => print_result(&result),
        Err(e) => eprintln!("FAILED: {e}"),
    }
}

fn print_result(result: &aust_core::models::RouteResult) {
    println!("Addresses:");
    for (i, addr) in result.addresses.iter().enumerate() {
        println!("  {}. {addr}", i + 1);
    }

    println!("\nLegs:");
    for (i, leg) in result.legs.iter().enumerate() {
        println!(
            "  Leg {}: {} → {}",
            i + 1,
            leg.from_address,
            leg.to_address
        );
        println!(
            "         {:.1} km, ~{} min",
            leg.distance_km, leg.duration_minutes
        );
    }

    println!("\nTotal: {:.1} km, ~{} min", result.total_distance_km, result.total_duration_minutes);
    println!("Price: €{:.2} (@ €{:.2}/km)", result.price_cents as f64 / 100.0, result.price_per_km_cents as f64 / 100.0);
}
