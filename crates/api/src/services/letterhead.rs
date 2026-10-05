//! Keeps the document generator's letterheads in step with `tenants`
//! (docs/MULTI_TENANT.md). Aust uses its compiled-in templates and is skipped.

use aust_core::tenant::{TenantId, AUST};
use aust_offer_generator::letterhead::Letterhead;
use aust_offer_generator::templates;
use sqlx::PgPool;

#[derive(sqlx::FromRow)]
struct Row {
    id: TenantId,
    name: String,
    short_name: String,
    owner_name: String,
    street: String,
    postal_code: String,
    city: String,
    phone: String,
    email: String,
    website: String,
    agb_url: String,
    bank_name: String,
    iban: String,
    bic: String,
    tax_number: String,
    vat_id: String,
    logo: Option<Vec<u8>>,
    accent_color: String,
}

const SELECT: &str = "SELECT id, name, short_name, owner_name, street, postal_code, city, phone, \
     email, website, agb_url, bank_name, iban, bic, tax_number, vat_id, logo, accent_color FROM tenants";

fn register(r: Row) {
    templates::register_letterhead(
        r.id,
        Letterhead {
            name: r.name,
            short_name: r.short_name,
            owner_name: r.owner_name,
            street: r.street,
            postal_code: r.postal_code,
            city: r.city,
            phone: r.phone,
            email: r.email,
            website: r.website,
            agb_url: r.agb_url,
            bank_name: r.bank_name,
            iban: r.iban,
            bic: r.bic,
            tax_number: r.tax_number,
            vat_id: r.vat_id,
            logo: r.logo,
            accent: r.accent_color,
        },
    );
}

/// Register every company's letterhead except Aust's. **Caller**: startup.
pub async fn register_all(db: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = aust_core::tenant::bypass(db).await?;
    let rows: Vec<Row> = sqlx::query_as(&format!("{SELECT} WHERE id <> $1"))
        .bind(AUST)
        .fetch_all(&mut *tx)
        .await?;
    rows.into_iter().for_each(register);
    Ok(())
}

/// Re-register one company's letterhead after its details or logo changed.
pub async fn refresh(db: &PgPool, tenant: TenantId) -> Result<(), sqlx::Error> {
    if tenant == AUST {
        return Ok(());
    }
    let mut tx = aust_core::tenant::bypass(db).await?;
    let row: Row = sqlx::query_as(&format!("{SELECT} WHERE id = $1"))
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await?;
    register(row);
    Ok(())
}

/// Accept a logo only if it is an image the generator can place (PNG, JPEG or
/// WebP, at most 2 MB).
pub fn validate_logo(bytes: &[u8]) -> Result<(), crate::ApiError> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(crate::ApiError::Validation("Logo höchstens 2 MB".into()));
    }
    aust_offer_generator::letterhead::check_logo(bytes)
        .map_err(|e| crate::ApiError::Validation(format!("Logo: {e}")))
}
