//! Admin repository — centralised queries for admin dashboard, customer management,
//! email threads, users, orders, and notes.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

/// Count open inquiries (pending, info_requested, estimated).
pub(crate) async fn count_open_inquiries(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM inquiries WHERE status IN ('pending', 'info_requested', 'estimated')",
    )
    .fetch_one(pool)
    .await?;
    Ok(count)
}

/// Count pending (draft) offers.
pub(crate) async fn count_pending_offers(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM offers WHERE status = 'draft'")
            .fetch_one(pool)
            .await?;
    Ok(count)
}

/// Count today's bookings (non-cancelled inquiries scheduled for today).
pub(crate) async fn count_todays_bookings(pool: &PgPool, today: NaiveDate) -> Result<i64, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM inquiries WHERE scheduled_date = $1 AND status NOT IN ('cancelled', 'rejected', 'expired')",
    )
    .bind(today)
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or((0,)).0)
}

/// Count total customers.
pub(crate) async fn count_total_customers(pool: &PgPool) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM customers")
        .fetch_one(pool)
        .await?;
    Ok(count)
}

/// Dashboard activity item row.
#[derive(Debug, FromRow)]
pub(crate) struct ActivityItem {
    pub activity_type: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub id: Option<Uuid>,
    pub status: Option<String>,
}

/// Fetch recent activity for the dashboard.
pub(crate) async fn fetch_recent_activity(pool: &PgPool) -> Result<Vec<ActivityItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT activity_type, description, created_at, id, status
        FROM (
            SELECT
                'inquiry' AS activity_type,
                COALESCE(c.name, c.email) || ' — ' || i.status AS description,
                i.updated_at AS created_at,
                i.id AS id,
                i.status AS status
            FROM inquiries i
            JOIN customers c ON i.customer_id = c.id
            WHERE i.status NOT IN ('cancelled', 'rejected', 'expired', 'paid')

            UNION ALL

            SELECT
                'offer_' || o.status AS activity_type,
                COALESCE(c.name, c.email) || ' — ' || round((o.price_cents::numeric / 100 * 1.19), 2)::text || ' € brutto' AS description,
                o.created_at AS created_at,
                q.id AS id,
                o.status AS status
            FROM offers o
            JOIN inquiries q ON o.inquiry_id = q.id
            JOIN customers c ON q.customer_id = c.id

            UNION ALL

            SELECT
                'email' AS activity_type,
                COALESCE(c.name, c.email) || ': ' || COALESCE(et.subject, '(kein Betreff)') AS description,
                et.updated_at AS created_at,
                et.id AS id,
                'unanswered' AS status
            FROM email_threads et
            JOIN customers c ON et.customer_id = c.id
            WHERE (
                SELECT direction FROM email_messages
                WHERE thread_id = et.id
                ORDER BY created_at DESC LIMIT 1
            ) = 'inbound'

            UNION ALL

            SELECT
                'calendar_item' AS activity_type,
                ci.title || COALESCE(' @ ' || ci.location, '') AS description,
                ci.created_at AS created_at,
                ci.id AS id,
                ci.status AS status
            FROM calendar_items ci
            WHERE ci.status = 'scheduled'
              AND (ci.scheduled_date IS NULL OR ci.scheduled_date >= CURRENT_DATE)
        ) combined
        ORDER BY created_at DESC
        LIMIT 15
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Conflict row for dates exceeding capacity.
#[derive(FromRow)]
pub(crate) struct ConflictRow {
    pub booking_date: NaiveDate,
    pub booking_count: i64,
}

/// Fetch dates in a range where bookings exceed capacity.
pub(crate) async fn fetch_conflict_dates(
    pool: &PgPool,
    from_date: NaiveDate,
    to_date: NaiveDate,
    default_capacity: i32,
) -> Result<Vec<ConflictRow>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT scheduled_date AS booking_date, COUNT(*) AS booking_count
        FROM inquiries
        WHERE scheduled_date BETWEEN $1 AND $2
          AND status NOT IN ('cancelled', 'rejected', 'expired')
        GROUP BY scheduled_date
        HAVING COUNT(*) > COALESCE(
            (SELECT capacity FROM calendar_capacity_overrides WHERE override_date = scheduled_date),
            $3
        )
        ORDER BY booking_date
        "#,
    )
    .bind(from_date)
    .bind(to_date)
    .bind(default_capacity)
    .fetch_all(pool)
    .await
}

/// Fetch capacity override for a specific date.
pub(crate) async fn fetch_capacity_override(
    pool: &PgPool,
    date: NaiveDate,
) -> Result<Option<i32>, sqlx::Error> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT capacity FROM calendar_capacity_overrides WHERE override_date = $1",
    )
    .bind(date)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|c| c.0))
}

// ---------------------------------------------------------------------------
// Customers
// ---------------------------------------------------------------------------

/// Customer list item row.
#[derive(Debug, FromRow)]
pub(crate) struct CustomerListItem {
    pub id: Uuid,
    pub email: Option<String>,
    pub name: Option<String>,
    pub salutation: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub phone: Option<String>,
    #[sqlx(default)]
    pub customer_type: Option<String>,
    #[sqlx(default)]
    pub company_name: Option<String>,
    #[sqlx(default)]
    pub billing_address_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// List customers with search filter.
pub(crate) async fn list_customers(
    pool: &PgPool,
    search: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<CustomerListItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, created_at
        FROM customers
        WHERE name ILIKE $1 OR email ILIKE $1
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(search)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Count customers matching search.
pub(crate) async fn count_customers(pool: &PgPool, search: &str) -> Result<i64, sqlx::Error> {
    let (total,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM customers WHERE name ILIKE $1 OR email ILIKE $1",
    )
    .bind(search)
    .fetch_one(pool)
    .await?;
    Ok(total)
}

/// Fetch a customer by ID (admin detail).
pub(crate) async fn fetch_customer(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CustomerListItem>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, billing_address_id, created_at FROM customers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Customer quote row for detail page.
#[derive(Debug, FromRow)]
pub(crate) struct CustomerQuote {
    pub id: Uuid,
    pub status: String,
    #[sqlx(default)]
    pub service_type: Option<String>,
    pub estimated_volume_m3: Option<f64>,
    pub scheduled_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

/// Fetch inquiries for a customer.
pub(crate) async fn fetch_customer_quotes(
    pool: &PgPool,
    customer_id: Uuid,
) -> Result<Vec<CustomerQuote>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, status, service_type, estimated_volume_m3, scheduled_date, created_at
        FROM inquiries WHERE customer_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(customer_id)
    .fetch_all(pool)
    .await
}

/// Customer offer row for detail page.
#[derive(Debug, FromRow)]
pub(crate) struct CustomerOffer {
    pub id: Uuid,
    pub inquiry_id: Uuid,
    pub price_cents: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

/// Fetch offers for a customer.
pub(crate) async fn fetch_customer_offers(
    pool: &PgPool,
    customer_id: Uuid,
) -> Result<Vec<CustomerOffer>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT o.id, o.inquiry_id, o.price_cents, o.status, o.created_at, o.sent_at
        FROM offers o
        JOIN inquiries q ON o.inquiry_id = q.id
        WHERE q.customer_id = $1
        ORDER BY o.created_at DESC
        "#,
    )
    .bind(customer_id)
    .fetch_all(pool)
    .await
}

/// Customer Termin (calendar item) row for detail page.
#[derive(Debug, FromRow)]
pub(crate) struct CustomerTermin {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    pub scheduled_date: Option<NaiveDate>,
    pub status: String,
}

/// Fetch calendar items (Termine) for a customer.
pub(crate) async fn fetch_customer_termine(
    pool: &PgPool,
    customer_id: Uuid,
) -> Result<Vec<CustomerTermin>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, title, category, scheduled_date, status
        FROM calendar_items WHERE customer_id = $1
        ORDER BY scheduled_date DESC NULLS LAST
        "#,
    )
    .bind(customer_id)
    .fetch_all(pool)
    .await
}

/// Update customer fields (partial update).
pub(crate) async fn update_customer(
    pool: &PgPool,
    id: Uuid,
    name: Option<&str>,
    salutation: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    phone: Option<&str>,
    email: Option<&str>,
    customer_type: Option<&str>,
    company_name: Option<&str>,
    // `None` = don't touch, `Some(Some(id))` = set, `Some(None)` = clear
    billing_address_id: Option<Option<Uuid>>,
) -> Result<Option<CustomerListItem>, sqlx::Error> {
    // $10 = do_update_billing: true if caller wants to change billing_address_id
    // $11 = new_billing_address_id: the value to set (NULL to clear)
    // $12 = do_update_email: true if caller wants to change email
    // $13 = new_email: the value to set (NULL to clear)
    // When email is provided in the request (even as empty string), we update it.
    // When email is omitted (None), we leave it unchanged.
    let do_update_billing = billing_address_id.is_some();
    let new_billing = billing_address_id.flatten();

    // Treat the email update similarly: Option<&str> where None means
    // "don't touch" and Some(value) means "set to this value".
    // An empty string Some("") means "clear the email".
    let do_update_email = email.is_some();
    let new_email: Option<&str> = email.filter(|s| !s.is_empty());

    sqlx::query_as(
        r#"
        UPDATE customers SET
            name = COALESCE($2, name),
            salutation = COALESCE($3, salutation),
            first_name = COALESCE($4, first_name),
            last_name = COALESCE($5, last_name),
            phone = COALESCE($6, phone),
            email = CASE WHEN $7 THEN $8 ELSE email END,
            customer_type = COALESCE($9, customer_type),
            company_name = COALESCE($10, company_name),
            billing_address_id = CASE WHEN $11 THEN $12 ELSE billing_address_id END
        WHERE id = $1
        RETURNING id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, billing_address_id, created_at
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(salutation)
    .bind(first_name)
    .bind(last_name)
    .bind(phone)
    .bind(do_update_email)
    .bind(new_email)
    .bind(customer_type)
    .bind(company_name)
    .bind(do_update_billing)
    .bind(new_billing)
    .fetch_optional(pool)
    .await
}

/// Create a new customer.
///
/// `email` is optional — when `None`, the customer is created without an email
/// address (useful for walk-in or phone customers who don't have email).
pub(crate) async fn create_customer(
    pool: &PgPool,
    id: Uuid,
    email: Option<&str>,
    name: Option<&str>,
    salutation: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
    phone: Option<&str>,
    customer_type: Option<&str>,
    company_name: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Option<CustomerListItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        INSERT INTO customers (id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, 'private'), $9, $10, $10)
        RETURNING id, email, name, salutation, first_name, last_name, phone, customer_type, company_name, billing_address_id, created_at
        "#,
    )
    .bind(id)
    .bind(email)
    .bind(name)
    .bind(salutation)
    .bind(first_name)
    .bind(last_name)
    .bind(phone)
    .bind(customer_type)
    .bind(company_name)
    .bind(now)
    .fetch_optional(pool)
    .await
}

/// Hard-delete a customer.
pub(crate) async fn delete_customer(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM customers WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

/// Order list item row.
#[derive(Debug, FromRow)]
pub(crate) struct OrderListItem {
    pub id: Uuid,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub origin_city: Option<String>,
    pub destination_city: Option<String>,
    pub estimated_volume_m3: Option<f64>,
    pub status: String,
    pub scheduled_date: Option<NaiveDate>,
    pub offer_price_brutto: Option<i64>,
    pub booking_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub employees_assigned: i64,
    pub employees_quoted: Option<i32>,
}

/// List orders with single status filter.
pub(crate) async fn list_orders_single_status(
    pool: &PgPool,
    status: &str,
    search: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<OrderListItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT q.id,
               c.name AS customer_name,
               c.email AS customer_email,
               oa.city AS origin_city,
               da.city AS destination_city,
               q.estimated_volume_m3,
               q.status,
               q.scheduled_date,
               (SELECT ROUND(o.price_cents * 1.19)::bigint FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_brutto,
               q.scheduled_date AS booking_date,
               q.created_at,
               (SELECT COUNT(DISTINCT employee_id)::bigint FROM inquiry_employees WHERE inquiry_id = q.id) AS employees_assigned,
               (SELECT o.persons FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS employees_quoted
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        WHERE q.status = $1
          AND (c.name ILIKE $2 OR c.email ILIKE $2)
        ORDER BY COALESCE(q.scheduled_date::timestamptz, q.created_at) ASC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(status)
    .bind(search)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// List orders with all order statuses.
pub(crate) async fn list_orders_all_statuses(
    pool: &PgPool,
    search: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<OrderListItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT q.id,
               c.name AS customer_name,
               c.email AS customer_email,
               oa.city AS origin_city,
               da.city AS destination_city,
               q.estimated_volume_m3,
               q.status,
               q.scheduled_date,
               (SELECT ROUND(o.price_cents * 1.19)::bigint FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_brutto,
               q.scheduled_date AS booking_date,
               q.created_at,
               (SELECT COUNT(DISTINCT employee_id)::bigint FROM inquiry_employees WHERE inquiry_id = q.id) AS employees_assigned,
               (SELECT o.persons FROM offers o WHERE o.inquiry_id = q.id ORDER BY o.created_at DESC LIMIT 1) AS employees_quoted
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        LEFT JOIN addresses oa ON q.origin_address_id = oa.id
        LEFT JOIN addresses da ON q.destination_address_id = da.id
        WHERE q.status IN ('accepted', 'scheduled', 'completed', 'invoiced', 'paid')
          AND (c.name ILIKE $1 OR c.email ILIKE $1)
        ORDER BY COALESCE(q.scheduled_date::timestamptz, q.created_at) ASC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(search)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Count orders with single status filter.
pub(crate) async fn count_orders_single_status(
    pool: &PgPool,
    status: &str,
    search: &str,
) -> Result<i64, sqlx::Error> {
    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        WHERE q.status = $1
          AND (c.name ILIKE $2 OR c.email ILIKE $2)
        "#,
    )
    .bind(status)
    .bind(search)
    .fetch_one(pool)
    .await?;
    Ok(total)
}

/// Count orders with all order statuses.
pub(crate) async fn count_orders_all_statuses(
    pool: &PgPool,
    search: &str,
) -> Result<i64, sqlx::Error> {
    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM inquiries q
        JOIN customers c ON q.customer_id = c.id
        WHERE q.status IN ('accepted', 'scheduled', 'completed', 'invoiced', 'paid')
          AND (c.name ILIKE $1 OR c.email ILIKE $1)
        "#,
    )
    .bind(search)
    .fetch_one(pool)
    .await?;
    Ok(total)
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// Address response row.
#[derive(Debug, FromRow)]
pub(crate) struct AddressResponse {
    pub id: Uuid,
    pub street: String,
    pub house_number: Option<String>,
    pub city: String,
    pub postal_code: Option<String>,
    pub floor: Option<String>,
    pub elevator: Option<bool>,
    pub parking_ban: bool,
}

/// Update an address (partial update).
pub(crate) async fn update_address(
    pool: &PgPool,
    id: Uuid,
    street: Option<&str>,
    house_number: Option<&str>,
    city: Option<&str>,
    postal_code: Option<&str>,
    floor: Option<&str>,
    elevator: Option<bool>,
    parking_ban: Option<bool>,
) -> Result<Option<AddressResponse>, sqlx::Error> {
    sqlx::query_as(
        r#"
        UPDATE addresses SET
            street = COALESCE($2, street),
            house_number = COALESCE($3, house_number),
            city = COALESCE($4, city),
            postal_code = COALESCE($5, postal_code),
            floor = COALESCE($6, floor),
            elevator = COALESCE($7, elevator),
            parking_ban = COALESCE($8, parking_ban)
        WHERE id = $1
        RETURNING id, street, house_number, city, postal_code, floor, elevator, parking_ban
        "#,
    )
    .bind(id)
    .bind(street)
    .bind(house_number)
    .bind(city)
    .bind(postal_code)
    .bind(floor)
    .bind(elevator)
    .bind(parking_ban)
    .fetch_optional(pool)
    .await
}

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// User list item row.
#[derive(Debug, FromRow)]
pub(crate) struct UserListItem {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

/// List all admin users.
pub(crate) async fn list_users(pool: &PgPool) -> Result<Vec<UserListItem>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, email, name, role, created_at FROM users ORDER BY created_at ASC",
    )
    .fetch_all(pool)
    .await
}

/// Hard-delete a user.
pub(crate) async fn delete_user(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Email Threads
// ---------------------------------------------------------------------------

/// Email thread list item row.
#[derive(Debug, FromRow)]
pub(crate) struct EmailThreadListItem {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_email: Option<String>,
    pub customer_name: Option<String>,
    pub inquiry_id: Option<Uuid>,
    pub subject: Option<String>,
    pub message_count: i64,
    pub last_message_at: Option<DateTime<Utc>>,
    pub last_direction: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// List email threads with search.
pub(crate) async fn list_email_threads(
    pool: &PgPool,
    search: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<EmailThreadListItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT
            et.id,
            et.customer_id,
            c.email AS customer_email,
            c.name AS customer_name,
            et.inquiry_id,
            et.subject,
            COUNT(em.id) AS message_count,
            MAX(em.created_at) AS last_message_at,
            (SELECT direction FROM email_messages
             WHERE thread_id = et.id ORDER BY created_at DESC LIMIT 1) AS last_direction,
            et.created_at
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        LEFT JOIN email_messages em ON em.thread_id = et.id
        WHERE c.name ILIKE $1 OR c.email ILIKE $1 OR et.subject ILIKE $1
        GROUP BY et.id, c.email, c.name
        ORDER BY MAX(em.created_at) DESC NULLS LAST
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(search)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Count email threads matching search.
pub(crate) async fn count_email_threads(pool: &PgPool, search: &str) -> Result<i64, sqlx::Error> {
    let (total,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(DISTINCT et.id)
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        WHERE c.name ILIKE $1 OR c.email ILIKE $1 OR et.subject ILIKE $1
        "#,
    )
    .bind(search)
    .fetch_one(pool)
    .await?;
    Ok(total)
}

/// Email thread detail row.
#[derive(Debug, FromRow)]
pub(crate) struct EmailThreadDetail {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub customer_email: Option<String>,
    pub customer_name: Option<String>,
    pub inquiry_id: Option<Uuid>,
    pub subject: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Fetch an email thread with customer info.
pub(crate) async fn fetch_email_thread(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<EmailThreadDetail>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT et.id, et.customer_id, c.email AS customer_email, c.name AS customer_name,
               et.inquiry_id, et.subject, et.created_at
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        WHERE et.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Email message item row.
#[derive(Debug, FromRow)]
pub(crate) struct EmailMessageItem {
    pub id: Uuid,
    pub direction: String,
    pub from_address: String,
    pub to_address: String,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub llm_generated: bool,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// Fetch messages for a thread (non-discarded).
pub(crate) async fn fetch_thread_messages(
    pool: &PgPool,
    thread_id: Uuid,
) -> Result<Vec<EmailMessageItem>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at
        FROM email_messages
        WHERE thread_id = $1 AND status != 'discarded'
        ORDER BY created_at ASC
        "#,
    )
    .bind(thread_id)
    .fetch_all(pool)
    .await
}

/// Fetch draft email with customer email and optional offer PDF.
pub(crate) async fn fetch_draft_for_send(
    pool: &PgPool,
    message_id: Uuid,
) -> Result<Option<(Option<String>, Option<String>, String, Option<String>, Option<Uuid>, Option<Uuid>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT em.subject, em.body_text, c.email,
               o.pdf_storage_key, o.id AS offer_id, et.inquiry_id
        FROM email_messages em
        JOIN email_threads et ON em.thread_id = et.id
        JOIN customers c ON et.customer_id = c.id
        LEFT JOIN offers o ON o.inquiry_id = et.inquiry_id
            AND o.status NOT IN ('rejected', 'cancelled')
        WHERE em.id = $1 AND em.status = 'draft'
        "#,
    )
    .bind(message_id)
    .fetch_optional(pool)
    .await
}

/// Update offer status to sent with timestamp.
pub(crate) async fn mark_offer_sent(
    pool: &PgPool,
    offer_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE offers SET status = 'sent', sent_at = $1 WHERE id = $2")
        .bind(now)
        .bind(offer_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Update inquiry status to offer_sent.
pub(crate) async fn mark_inquiry_offer_sent(
    pool: &PgPool,
    inquiry_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inquiries SET status = 'offer_sent', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(inquiry_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark a draft email message as sent and fix to_address.
pub(crate) async fn mark_message_sent(
    pool: &PgPool,
    message_id: Uuid,
    customer_email: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE email_messages SET status = 'sent', to_address = $2 WHERE id = $1")
        .bind(message_id)
        .bind(customer_email)
        .execute(pool)
        .await?;
    Ok(())
}

/// Discard a draft email message.
pub(crate) async fn discard_draft(pool: &PgPool, message_id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE email_messages SET status = 'discarded' WHERE id = $1 AND status = 'draft'",
    )
    .bind(message_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Update a draft email's subject and/or body.
pub(crate) async fn update_draft(
    pool: &PgPool,
    message_id: Uuid,
    subject: Option<&str>,
    body_text: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE email_messages SET subject = COALESCE($2, subject), body_text = COALESCE($3, body_text) WHERE id = $1 AND status = 'draft'",
    )
    .bind(message_id)
    .bind(subject)
    .bind(body_text)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Fetch thread info for reply (customer_id, email, subject).
pub(crate) async fn fetch_thread_for_reply(
    pool: &PgPool,
    thread_id: Uuid,
) -> Result<Option<(Uuid, String, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        r#"
        SELECT et.customer_id, c.email, et.subject
        FROM email_threads et
        JOIN customers c ON et.customer_id = c.id
        WHERE et.id = $1
        "#,
    )
    .bind(thread_id)
    .fetch_optional(pool)
    .await
}

/// Insert a draft reply message.
pub(crate) async fn insert_reply_draft(
    pool: &PgPool,
    id: Uuid,
    thread_id: Uuid,
    from_address: &str,
    to_address: &str,
    subject: Option<&str>,
    body_text: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO email_messages (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, 'outbound', $3, $4, $5, $6, false, 'draft', $7)
        "#,
    )
    .bind(id)
    .bind(thread_id)
    .bind(from_address)
    .bind(to_address)
    .bind(subject)
    .bind(body_text)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Upsert customer by email (compose flow — minimal, only sets updated_at).
pub(crate) async fn upsert_customer_for_compose(
    pool: &PgPool,
    email: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO customers (id, email, created_at, updated_at)
        VALUES ($1, $2, $3, $3)
        ON CONFLICT (email) DO UPDATE SET updated_at = $3
        RETURNING id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(email)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// Create a new email thread (compose flow).
pub(crate) async fn create_compose_thread(
    pool: &PgPool,
    id: Uuid,
    customer_id: Uuid,
    subject: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_threads (id, customer_id, subject, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(customer_id)
    .bind(subject)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a compose draft message.
pub(crate) async fn insert_compose_draft(
    pool: &PgPool,
    id: Uuid,
    thread_id: Uuid,
    from_address: &str,
    to_address: &str,
    subject: &str,
    body_text: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO email_messages (id, thread_id, direction, from_address, to_address, subject, body_text, llm_generated, status, created_at)
        VALUES ($1, $2, 'outbound', $3, $4, $5, $6, false, 'draft', $7)
        "#,
    )
    .bind(id)
    .bind(thread_id)
    .bind(from_address)
    .bind(to_address)
    .bind(subject)
    .bind(body_text)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

/// Note row.
#[derive(Debug, FromRow)]
pub(crate) struct NoteRow {
    pub id: Uuid,
    pub title: String,
    pub content: String,
    pub color: String,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// List all notes.
pub(crate) async fn list_notes(pool: &PgPool) -> Result<Vec<NoteRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, title, content, color, pinned, created_at, updated_at
         FROM notes ORDER BY pinned DESC, created_at DESC",
    )
    .fetch_all(pool)
    .await
}

/// Create a note.
pub(crate) async fn create_note(
    pool: &PgPool,
    id: Uuid,
    title: &str,
    content: &str,
    color: &str,
    pinned: bool,
) -> Result<NoteRow, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO notes (id, title, content, color, pinned)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, title, content, color, pinned, created_at, updated_at",
    )
    .bind(id)
    .bind(title)
    .bind(content)
    .bind(color)
    .bind(pinned)
    .fetch_one(pool)
    .await
}

/// Update a note (partial update).
pub(crate) async fn update_note(
    pool: &PgPool,
    id: Uuid,
    title: Option<&str>,
    content: Option<&str>,
    color: Option<&str>,
    pinned: Option<bool>,
) -> Result<Option<NoteRow>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE notes
         SET title   = COALESCE($2, title),
             content = COALESCE($3, content),
             color   = COALESCE($4, color),
             pinned  = COALESCE($5, pinned)
         WHERE id = $1
         RETURNING id, title, content, color, pinned, created_at, updated_at",
    )
    .bind(id)
    .bind(title)
    .bind(content)
    .bind(color)
    .bind(pinned)
    .fetch_optional(pool)
    .await
}

/// Delete a note.
pub(crate) async fn delete_note(pool: &PgPool, id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Morning workflow
// ---------------------------------------------------------------------------

/// Inquiry row returned by the morning-workflow query.
///
/// Includes the last working day, current status, latest invoice info,
/// and whether a review-request was already handled.
#[derive(Debug, FromRow)]
pub(crate) struct MorningInquiryRow {
    pub id: Uuid,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub last_day: Option<NaiveDate>,
    pub status: String,
    pub invoice_status: Option<String>,
    pub invoice_id: Option<Uuid>,
    pub invoice_type: Option<String>,
    pub has_review_request: bool,
    pub offer_price_cents: Option<i64>,
}

/// Calendar-item row returned by the morning-workflow query.
#[derive(Debug, FromRow)]
pub(crate) struct MorningCalendarItemRow {
    pub id: Uuid,
    pub title: String,
    pub last_day: Option<NaiveDate>,
    pub status: String,
}

/// Returns inquiries whose last working day has already passed (within 14 days)
/// but that still need action: not yet marked complete, invoice not sent, or review pending.
///
/// **Caller**: `routes::admin::morning_workflow`
/// **Why**: Powers the "morning checklist" dialog — shown to admins on dashboard load.
pub(crate) async fn fetch_morning_inquiries(pool: &PgPool) -> Result<Vec<MorningInquiryRow>, sqlx::Error> {
    sqlx::query_as::<_, MorningInquiryRow>(
        r#"
        WITH last_inquiry_days AS (
            SELECT id AS inquiry_id, COALESCE(end_date, scheduled_date) AS last_day
            FROM inquiries
        ),
        latest_invoices AS (
            SELECT DISTINCT ON (inquiry_id)
                inquiry_id, id, status, invoice_type
            FROM invoices
            ORDER BY inquiry_id, created_at DESC
        )
        SELECT
            i.id,
            c.name AS customer_name,
            c.email AS customer_email,
            COALESCE(ld.last_day, i.scheduled_date)        AS last_day,
            i.status,
            li.status                                       AS invoice_status,
            li.id                                           AS invoice_id,
            li.invoice_type,
            EXISTS(
                SELECT 1 FROM review_requests rr
                WHERE rr.inquiry_id = i.id
                  AND rr.status IN ('sent', 'skipped')
            )                                               AS has_review_request,
            (SELECT price_cents FROM offers o WHERE o.inquiry_id = i.id ORDER BY o.created_at DESC LIMIT 1) AS offer_price_cents
        FROM inquiries i
        LEFT JOIN customers c                ON c.id = i.customer_id
        LEFT JOIN last_inquiry_days ld       ON ld.inquiry_id = i.id
        LEFT JOIN latest_invoices li         ON li.inquiry_id = i.id
        WHERE i.status IN ('scheduled', 'accepted', 'completed', 'invoiced')
          AND COALESCE(ld.last_day, i.scheduled_date) < CURRENT_DATE
          AND COALESCE(ld.last_day, i.scheduled_date) >= CURRENT_DATE - INTERVAL '14 days'
          AND NOT (
              i.status IN ('invoiced', 'paid')
              AND EXISTS(
                  SELECT 1 FROM review_requests rr
                  WHERE rr.inquiry_id = i.id AND rr.status IN ('sent', 'skipped')
              )
          )
        ORDER BY COALESCE(ld.last_day, i.scheduled_date) DESC
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Returns calendar items whose last working day has already passed (within 14 days)
/// and whose status is still 'scheduled' (not yet marked complete).
///
/// **Caller**: `routes::admin::morning_workflow`
pub(crate) async fn fetch_morning_calendar_items(pool: &PgPool) -> Result<Vec<MorningCalendarItemRow>, sqlx::Error> {
    sqlx::query_as::<_, MorningCalendarItemRow>(
        r#"
        SELECT
            ci.id,
            ci.title,
            COALESCE(ci.end_date, ci.scheduled_date) AS last_day,
            ci.status
        FROM calendar_items ci
        WHERE ci.status = 'scheduled'
          AND COALESCE(ci.end_date, ci.scheduled_date) < CURRENT_DATE
          AND COALESCE(ci.end_date, ci.scheduled_date) >= CURRENT_DATE - INTERVAL '14 days'
        ORDER BY COALESCE(ci.end_date, ci.scheduled_date) DESC
        "#,
    )
    .fetch_all(pool)
    .await
}
