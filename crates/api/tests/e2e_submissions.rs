//! End-to-end tests for foto-angebot submission endpoints.
//!
//! Covers all Rust-backend modes (photo, video, manual) across every service type
//! that uses them, plus edge cases (business customer, empty addons, volume fast-path).
//!
//! Termin-only services (Lagerung, Umzugshelfer) flow through PHP → email → IMAP
//! and are intentionally NOT tested here — they never hit Rust submission handlers.

use aust_api::test_helpers;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

// =============================================================================
// Helpers
// =============================================================================

/// Build a router bound to the provided test DB pool.
async fn build_router(pool: PgPool) -> axum::Router {
    let state = test_helpers::test_app_state_with_pool(pool).await;
    aust_api::create_router(state)
}

/// Extract JSON body from an Axum response.
async fn body_json(resp: axum::http::Response<Body>) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Build a multipart body manually so we don't need an external crate.
fn multipart_body(
    fields: &[(String, String)],
    files: &[(String, Vec<u8>, String, String)], // field_name, bytes, filename, content_type
    boundary: &str,
) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    for (field_name, bytes, filename, content_type) in files {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field_name}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                filename, content_type
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

/// Standard field set for a residential-move submission.
fn standard_fields(
    service_type: &str,
    submission_mode: &str,
    addons: &[&str],
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = vec![
        ("email".into(), "e2e-test@example.com".into()),
        ("name".into(), "Max Mustermann".into()),
        ("first_name".into(), "Max".into()),
        ("last_name".into(), "Mustermann".into()),
        ("salutation".into(), "Herr".into()),
        ("phone".into(), "+491234567890".into()),
        ("departure_address".into(), "Musterstr. 1".into()),
        ("startOrt".into(), "Hildesheim".into()),
        ("startPlz".into(), "31134".into()),
        ("etage_auszug".into(), "2. Stock".into()),
        ("aufzug_auszug".into(), "true".into()),
        ("halteverbot_auszug".into(), "true".into()),
        ("arrival_address".into(), "Zielstr. 5".into()),
        ("endOrt".into(), "Hannover".into()),
        ("endPlz".into(), "30159".into()),
        ("etage_einzug".into(), "Erdgeschoss".into()),
        ("aufzug_einzug".into(), "false".into()),
        ("halteverbot_einzug".into(), "false".into()),
        ("scheduled_date".into(), "2026-07-01".into()),
        ("service_type".into(), service_type.into()),
        ("submission_mode".into(), submission_mode.into()),
        ("customer_type".into(), "private".into()),
        ("message".into(), "E2E-Test Nachricht".into()),
    ];
    if !addons.is_empty() {
        fields.push(("services".into(), addons.join(",")));
    }
    fields.extend(extra.iter().cloned());
    fields
}

/// Assert the inquiry exists with the expected mode / service.
async fn assert_inquiry_exists(pool: &PgPool, inquiry_id: uuid::Uuid, mode: &str, service: &str) {
    let row: (String, String, String, String, Option<String>, String) = sqlx::query_as(
        "SELECT i.submission_mode, i.service_type, i.status, i.source,
                i.scheduled_date::text, c.email
         FROM inquiries i JOIN customers c ON c.id = i.customer_id
         WHERE i.id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await
    .expect("inquiry must exist");

    assert_eq!(row.0, mode, "submission_mode mismatch");
    assert_eq!(row.1, service, "service_type mismatch");
    assert!(matches!(row.2.as_str(), "pending" | "estimated"), "unexpected status");
    assert_eq!(row.5, "e2e-test@example.com");
}

/// Assert origin/destination addresses are persisted correctly.
async fn assert_addresses(pool: &PgPool, inquiry_id: uuid::Uuid) {
    let origin: (String, String, String, Option<String>, Option<bool>, Option<bool>) = sqlx::query_as(
        "SELECT a.street, a.city, a.postal_code, a.floor, a.elevator, a.parking_ban
         FROM addresses a JOIN inquiries i ON i.origin_address_id = a.id
         WHERE i.id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await
    .expect("origin address");

    assert!(origin.0.to_lowercase().contains("musterstr"), "origin street: {}", origin.0);
    assert_eq!(origin.1, "Hildesheim");
    assert_eq!(origin.2, "31134");
    // floor stored as int: "2. Stock" → Some("2")  (depends on DB parse, but assert it's Some)
    assert!(origin.3.is_some(), "origin floor must be set");
    assert_eq!(origin.4, Some(true), "origin elevator");
    assert_eq!(origin.5, Some(true), "origin parking_ban");

    let dest: (String, String, String, Option<String>, Option<bool>, Option<bool>) = sqlx::query_as(
        "SELECT a.street, a.city, a.postal_code, a.floor, a.elevator, a.parking_ban
         FROM addresses a JOIN inquiries i ON i.destination_address_id = a.id
         WHERE i.id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await
    .expect("destination address");

    assert!(dest.0.to_lowercase().contains("zielstr"), "dest street: {}", dest.0);
    assert_eq!(dest.1, "Hannover");
    assert_eq!(dest.2, "30159");
    assert_eq!(dest.4, Some(false), "dest elevator");
    assert_eq!(dest.5, Some(false), "dest parking_ban");
}

/// Assert services JSONB flags.
async fn assert_services(pool: &PgPool, inquiry_id: uuid::Uuid, expected_additions: &[&str]) {
    let services: sqlx::types::Json<Value> = sqlx::query_scalar(
        "SELECT services FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(pool)
    .await
    .expect("services queryable");

    let s = services.0;
    for addon in expected_additions {
        let key = match *addon {
            "packing" | "einpack" | "verpackung" | "Einpackservice" => "packing",
            "assembly" | "montage" | "Möbelmontage" => "assembly",
            "disassembly" | "demontage" | "Möbeldemontage" => "disassembly",
            "storage" | "einlagerung" | "Einlagerung" => "storage",
            "disposal" | "entsorgung" | "Entsorgung" | "Entsorgung Sperrmüll" => "disposal",
            _ => addon,
        };
        assert!(
            s[key].as_bool().unwrap_or(false),
            "service flag '{key}' expected true for addon '{addon}'"
        );
    }
}

/// Minimal JPEG header for upload tests.
fn fake_jpeg() -> Vec<u8> {
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
        0x00, 0x01, 0x00, 0x01, 0x00, 0x00,
    ]
}

/// Minimal MP4 header.
fn fake_mp4() -> Vec<u8> {
    vec![
        0x00, 0x00, 0x00, 0x18, 0x66, 0x74, 0x79, 0x70, 0x6D, 0x70, 0x34, 0x32,
    ]
}

// =============================================================================
// Photo submissions
// =============================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn photo_submission_privatumzug_with_all_addons(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryPhoto";
    let fields = standard_fields(
        "privatumzug",
        "foto",
        &["Einpackservice", "Möbelmontage", "Möbeldemontage", "Einlagerung", "Entsorgung Sperrmüll"],
        &[],
    );
    let body = multipart_body(
        &fields,
        &[(
            "images".into(),
            fake_jpeg(),
            "wohnzimmer.jpg".into(),
            "image/jpeg".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/photo")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();

    assert_inquiry_exists(&pool, inquiry_id, "foto", "privatumzug").await;
    assert_addresses(&pool, inquiry_id).await;
    assert_services(
        &pool,
        inquiry_id,
        &["Einpackservice", "Möbelmontage", "Möbeldemontage", "Einlagerung", "Entsorgung Sperrmüll"],
    )
    .await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn photo_submission_firmenumzug_business_customer(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryBiz";
    let fields = standard_fields("firmenumzug", "foto", &["Einpackservice"], &[
        ("customer_type".into(), "business".into()),
        ("company_name".into(), "Acme GmbH".into()),
    ]);
    let body = multipart_body(
        &fields,
        &[(
            "images".into(),
            fake_jpeg(),
            "buero.jpg".into(),
            "image/jpeg".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/photo")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();

    assert_inquiry_exists(&pool, inquiry_id, "foto", "firmenumzug").await;

    let customer: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT customer_type, company_name FROM customers
         WHERE email = $1",
    )
    .bind("e2e-test@example.com")
    .fetch_one(&pool)
    .await
    .expect("customer must exist");

    assert_eq!(customer.0.as_deref(), Some("business"));
    assert_eq!(customer.1.as_deref(), Some("Acme GmbH"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn photo_submission_seniorenumzug(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundarySenior";
    let fields = standard_fields("seniorenumzug", "foto", &[], &[]);
    let body = multipart_body(
        &fields,
        &[(
            "images".into(),
            fake_jpeg(),
            "zimmer.jpg".into(),
            "image/jpeg".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/photo")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();
    assert_inquiry_exists(&pool, inquiry_id, "foto", "seniorenumzug").await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn photo_submission_montage_single_address(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryMontage";
    let fields = standard_fields("montage", "foto", &[], &[]);
    let body = multipart_body(
        &fields,
        &[(
            "images".into(),
            fake_jpeg(),
            "moebel.jpg".into(),
            "image/jpeg".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/photo")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();
    assert_inquiry_exists(&pool, inquiry_id, "foto", "montage").await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn photo_submission_haushaltsaufloesung(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryHA";
    let fields = standard_fields(
        "haushaltsaufloesung",
        "foto",
        &["Wertankauf", "Entsorgung Sperrmüll"],
        &[
            ("billing_street".into(), "Rechnungsstr. 7".into()),
            ("billing_zip".into(), "10115".into()),
            ("billing_city".into(), "Berlin".into()),
        ],
    );
    let body = multipart_body(
        &fields,
        &[(
            "images".into(),
            fake_jpeg(),
            "haus.jpg".into(),
            "image/jpeg".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/photo")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();
    assert_inquiry_exists(&pool, inquiry_id, "foto", "haushaltsaufloesung").await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn photo_submission_entruempelung(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryEnt";
    let fields = standard_fields("entruempelung", "foto", &["Wertankauf"], &[]);
    let body = multipart_body(
        &fields,
        &[(
            "images".into(),
            fake_jpeg(),
            "keller.jpg".into(),
            "image/jpeg".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/photo")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();
    assert_inquiry_exists(&pool, inquiry_id, "foto", "entruempelung").await;
}

// =============================================================================
// Video submissions
// =============================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn video_submission_privatumzug(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryVideo";
    let fields = standard_fields("privatumzug", "video", &["Möbeldemontage"], &[]);
    let body = multipart_body(
        &fields,
        &[(
            "video".into(),
            fake_mp4(),
            "rundgang.mp4".into(),
            "video/mp4".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/video")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();
    assert_inquiry_exists(&pool, inquiry_id, "video", "privatumzug").await;
    assert_services(&pool, inquiry_id, &["Möbeldemontage"]).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn video_submission_firmenumzug(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryVideoBiz";
    let fields = standard_fields("firmenumzug", "video", &[], &[
        ("customer_type".into(), "business".into()),
        ("company_name".into(), "TechStart GmbH".into()),
    ]);
    let body = multipart_body(
        &fields,
        &[(
            "video".into(),
            fake_mp4(),
            "buerorundgang.mp4".into(),
            "video/mp4".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/video")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn video_submission_seniorenumzug(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryVideoSen";
    let fields = standard_fields("seniorenumzug", "video", &[], &[]);
    let body = multipart_body(
        &fields,
        &[(
            "video".into(),
            fake_mp4(),
            "seniorenwohnung.mp4".into(),
            "video/mp4".into(),
        )],
        boundary,
    );

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/video")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

// =============================================================================
// Manual submissions
// =============================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn manual_submission_privatumzug_with_volume(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryManual";
    let fields = standard_fields("privatumzug", "manuell", &["Einpackservice"], &[
        ("volumen".into(), "48.5".into()),
        ("umzugsgut".into(), "Sofa, Schrank, Küchenzeile".into()),
    ]);
    let body = multipart_body(&fields, &[], boundary);

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/manual")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    // With volume → status goes to estimated
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM inquiries WHERE id = $1")
        .bind(inquiry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "estimated", "manual with volume advances to estimated");

    let vol: Option<f64> = sqlx::query_scalar("SELECT estimated_volume_m3 FROM inquiries WHERE id = $1")
        .bind(inquiry_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(vol, Some(48.5), "volume must be stored");

    assert_addresses(&pool, inquiry_id).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_submission_firmenumzug(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryManualBiz";
    let fields = standard_fields("firmenumzug", "manuell", &[], &[
        ("customer_type".into(), "business".into()),
        ("company_name".into(), "MegaCorp AG".into()),
    ]);
    let body = multipart_body(&fields, &[], boundary);

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/manual")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED); // no volume → stays pending
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_submission_seniorenumzug(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryManualSen";
    let fields = standard_fields("seniorenumzug", "manuell", &[], &[]);
    let body = multipart_body(&fields, &[], boundary);

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/manual")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
}

// =============================================================================
// Edge cases
// =============================================================================

#[sqlx::test(migrations = "../../migrations")]
async fn manual_submission_no_addons(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryMin";
    let fields = standard_fields("privatumzug", "manuell", &[], &[]);
    let body = multipart_body(&fields, &[], boundary);

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/manual")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();

    let services: sqlx::types::Json<Value> = sqlx::query_scalar(
        "SELECT services FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    // empty addons → parking_ban still derived from booleans
    assert!(
        !services.0["packing"].as_bool().unwrap_or(false),
        "no addons → packing false"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_submission_scheduled_date_parsed(pool: PgPool) {
    let router = build_router(pool.clone()).await;
    let boundary = "E2EBoundaryDate";
    let fields = standard_fields("privatumzug", "manuell", &[], &[
        ("scheduled_date".into(), "2026-12-24".into()),
    ]);
    let body = multipart_body(&fields, &[], boundary);

    let resp = router
        .oneshot(
            Request::post("/api/v1/submit/manual")
                .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let json = body_json(resp).await;
    let inquiry_id: uuid::Uuid = json["inquiry_id"].as_str().unwrap().parse().unwrap();

    let date: Option<String> = sqlx::query_scalar(
        "SELECT scheduled_date::text FROM inquiries WHERE id = $1",
    )
    .bind(inquiry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(date, Some("2026-12-24".to_string()));
}
