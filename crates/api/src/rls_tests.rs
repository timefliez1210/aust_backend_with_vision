//! Row-level security keeps tenants apart (docs/MULTI_TENANT.md, step 3).
//!
//! The usual test database connects as a superuser, which skips row-level
//! security, so these tests then switch to a plain role (`rls_probe`) inside the
//! transaction. Run as a non-superuser app role (how prod will connect), they need
//! no switch.

use aust_core::tenant::{self, AUST};
use sqlx::PgPool;
use uuid::Uuid;

/// Every table that has a `tenant_id` is guarded, and `FORCE`d so the owning role
/// is guarded too. A new table without a policy fails here.
#[sqlx::test(migrations = "../../migrations")]
async fn every_tenant_table_has_row_level_security(pool: PgPool) {
    let unguarded: Vec<(String,)> = sqlx::query_as(
        "SELECT c.relname::text
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace AND n.nspname = 'public'
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attname = 'tenant_id' AND NOT a.attisdropped
         WHERE c.relkind = 'r'
           AND NOT (c.relrowsecurity AND c.relforcerowsecurity
                    AND EXISTS (SELECT 1 FROM pg_policy p
                                WHERE p.polrelid = c.oid AND p.polname = 'tenant_isolation'))
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(unguarded.is_empty(), "tables without row-level security: {unguarded:?}");

    let without_tenant: Vec<(String,)> = sqlx::query_as(
        "SELECT table_name::text FROM information_schema.tables t
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'
           AND table_name NOT IN ('_sqlx_migrations', 'tenants')
           AND NOT EXISTS (SELECT 1 FROM information_schema.columns c
                           WHERE c.table_schema = 'public' AND c.table_name = t.table_name
                             AND c.column_name = 'tenant_id')
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(without_tenant.is_empty(), "tables without tenant_id: {without_tenant:?}");
}

/// Make sure the test runs subject to row-level security: as a superuser (the
/// usual test database) switch to a plain role; as the app's own non-superuser
/// role it already is. Returns the role to `SET LOCAL` to, if any.
async fn probe_role(pool: &PgPool) -> Option<&'static str> {
    let (superuser,): (bool,) =
        sqlx::query_as("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
            .fetch_one(pool)
            .await
            .unwrap();
    if !superuser {
        return None;
    }
    sqlx::query(
        "DO $$ BEGIN CREATE ROLE rls_probe NOLOGIN;
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA public TO rls_probe").execute(pool).await.unwrap();
    sqlx::query("GRANT ALL ON ALL TABLES IN SCHEMA public TO rls_probe")
        .execute(pool)
        .await
        .unwrap();
    Some("rls_probe")
}

/// Seed a customer for any tenant (through the bypass, so it works under RLS too).
async fn customer(pool: &PgPool, tenant: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    let mut tx = tenant::bypass(pool).await.unwrap();
    sqlx::query("INSERT INTO customers (id, tenant_id, email, last_name) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(tenant)
        .bind(format!("{id}@example.com"))
        .bind(name)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

/// One company never sees, changes or creates another company's rows — and the
/// login bypass sees both.
#[sqlx::test(migrations = "../../migrations")]
async fn a_tenant_sees_only_its_own_rows(pool: PgPool) {
    let role = probe_role(&pool).await;
    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO tenants (id, slug, name) VALUES ($1, 'zweite', 'Zweite Umzüge')")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let ours = customer(&pool, AUST.0, "Aust-Kunde").await;
    let theirs = customer(&pool, other, "Fremdkunde").await;

    let mut tx = pool.begin().await.unwrap();
    if let Some(role) = role {
        sqlx::query(&format!("SET LOCAL ROLE {role}")).execute(&mut *tx).await.unwrap();
    }
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(AUST.0.to_string())
        .execute(&mut *tx)
        .await
        .unwrap();

    let visible: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM customers WHERE id = ANY($1)")
        .bind(vec![ours, theirs])
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(visible, vec![(ours,)]);

    let changed = sqlx::query("UPDATE customers SET last_name = 'x' WHERE id = $1")
        .bind(theirs)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    assert_eq!(changed, 0, "another tenant's row must not be writable");

    sqlx::query("SAVEPOINT s").execute(&mut *tx).await.unwrap();
    let smuggled = sqlx::query("INSERT INTO customers (id, tenant_id, email) VALUES ($1, $2, 'x@example.com')")
        .bind(Uuid::now_v7())
        .bind(other)
        .execute(&mut *tx)
        .await;
    assert!(smuggled.is_err(), "inserting into another tenant must fail");
    sqlx::query("ROLLBACK TO SAVEPOINT s").execute(&mut *tx).await.unwrap();

    // A row created without naming the tenant lands in the connection's tenant.
    let fresh = Uuid::now_v7();
    sqlx::query("INSERT INTO customers (id, email) VALUES ($1, 'neu@example.com')")
        .bind(fresh)
        .execute(&mut *tx)
        .await
        .unwrap();
    let (owner,): (Uuid,) = sqlx::query_as("SELECT tenant_id FROM customers WHERE id = $1")
        .bind(fresh)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(owner, AUST.0);

    sqlx::query("SELECT set_config('app.tenant_bypass', 'on', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let (both,): (i64,) = sqlx::query_as("SELECT count(*) FROM customers WHERE id = ANY($1)")
        .bind(vec![ours, theirs])
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(both, 2, "the bypass sees every tenant");
    tx.rollback().await.unwrap();
}

/// `tenant::bypass` opens exactly that bypass, and it ends with its transaction.
#[sqlx::test(migrations = "../../migrations")]
async fn bypass_is_scoped_to_its_transaction(pool: PgPool) {
    let mut tx = tenant::bypass(&pool).await.unwrap();
    let (on,): (bool,) = sqlx::query_as("SELECT tenant_bypass()").fetch_one(&mut *tx).await.unwrap();
    assert!(on);
    tx.commit().await.unwrap();
    let (after,): (bool,) = sqlx::query_as("SELECT tenant_bypass()").fetch_one(&pool).await.unwrap();
    assert!(!after);
}

/// KVA numbers: Aust keeps drawing from `offer_number_seq`; another company
/// counts on its own from 1001 and never takes one of Aust's numbers.
#[sqlx::test(migrations = "../../migrations")]
async fn each_company_numbers_its_own_kvas(
    opts: sqlx::postgres::PgPoolOptions,
    conn: sqlx::postgres::PgConnectOptions,
) {
    let pool = crate::tenant_aware(opts).connect_with(conn).await.unwrap();
    use crate::repositories::offer_repo::next_offer_number;
    let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO tenants (id, slug, name) VALUES ($1, 'zweite', 'Zweite Umzüge')")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();

    let aust_1 = tenant::scope(AUST, next_offer_number(&pool, day)).await.unwrap();
    let other_1 = tenant::scope(tenant::TenantId(other), next_offer_number(&pool, day)).await.unwrap();
    let other_2 = tenant::scope(tenant::TenantId(other), next_offer_number(&pool, day)).await.unwrap();
    let aust_2 = tenant::scope(AUST, next_offer_number(&pool, day)).await.unwrap();

    assert_eq!(other_1, "2026-1001");
    assert_eq!(other_2, "2026-1002");
    let n = |s: &str| s.split('-').nth(1).unwrap().parse::<i64>().unwrap();
    assert_eq!(n(&aust_2), n(&aust_1) + 1, "Aust's sequence must not jump");

    let owners: Vec<(Uuid,)> = sqlx::query_as("SELECT tenant_id FROM offer_number_counters")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(owners, vec![(other,)], "the counter row belongs to the other company");
}

/// Every single-column link between two tenant tables onto the parent's `id` has
/// a twin on (tenant_id, column), so a link cannot cross companies — foreign-key
/// checks ignore row-level security. A new table without the twin fails here.
#[sqlx::test(migrations = "../../migrations")]
async fn every_link_between_tenant_tables_stays_in_one_company(pool: PgPool) {
    let missing: Vec<(String,)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text || '.' || ca.attname
         FROM pg_constraint c
         JOIN pg_attribute ca ON ca.attrelid = c.conrelid  AND ca.attnum = c.conkey[1]
         JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = c.confkey[1]
         WHERE c.contype = 'f' AND c.connamespace = 'public'::regnamespace
           AND array_length(c.conkey, 1) = 1 AND pa.attname = 'id' AND ca.attname <> 'tenant_id'
           AND EXISTS (SELECT 1 FROM pg_attribute t WHERE t.attrelid = c.conrelid AND t.attname = 'tenant_id')
           AND EXISTS (SELECT 1 FROM pg_attribute t WHERE t.attrelid = c.confrelid AND t.attname = 'tenant_id')
           AND NOT EXISTS (
               SELECT 1 FROM pg_constraint t
               JOIN pg_attribute t1 ON t1.attrelid = t.conrelid AND t1.attnum = t.conkey[1]
               JOIN pg_attribute t2 ON t2.attrelid = t.conrelid AND t2.attnum = t.conkey[2]
               WHERE t.contype = 'f' AND t.conrelid = c.conrelid AND t.confrelid = c.confrelid
                 AND t1.attname = 'tenant_id' AND t2.attname = ca.attname)
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(missing.is_empty(), "links without a same-company twin: {missing:?}");
}

/// Another company's inquiry cannot point at an Aust customer — even when the
/// writer could get past row-level security (bypass, superuser).
#[sqlx::test(migrations = "../../migrations")]
async fn a_link_to_another_companys_row_is_refused(pool: PgPool) {
    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO tenants (id, slug, name) VALUES ($1, 'zweite', 'Zweite')")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let aust_customer = customer(&pool, AUST.0, "Aust-Kunde").await;
    let crossed = sqlx::query(
        "INSERT INTO inquiries (id, tenant_id, customer_id, status) VALUES ($1, $2, $3, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(other)
    .bind(aust_customer)
    .execute(&pool)
    .await;
    let err = crossed.expect_err("a cross-company link must be refused").to_string();
    assert!(err.contains("same_tenant"), "refused by the twin key: {err}");
}

/// Two companies keep their own daily-briefing slots and their own binding for
/// the same Telegram chat (a private chat has one id across all bots).
#[sqlx::test(migrations = "../../migrations")]
async fn companies_share_slots_and_chats_without_colliding(pool: PgPool) {
    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO tenants (id, slug, name) VALUES ($1, 'zweite', 'Zweite')")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
    for t in [AUST.0, other] {
        let claimed: Option<(chrono::NaiveDate,)> = sqlx::query_as(
            "INSERT INTO agent_briefing_log (tenant_id, slot_date, slot, chat_id) VALUES ($1, $2, 'morning', 42)
             ON CONFLICT (tenant_id, slot_date, slot) DO NOTHING RETURNING slot_date",
        )
        .bind(t)
        .bind(day)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(claimed.is_some(), "each company claims its own morning slot");
        sqlx::query("INSERT INTO agent_sessions (id, tenant_id, chat_id, turns) VALUES ($1, $2, 4242, '[]')")
            .bind(Uuid::now_v7())
            .bind(t)
            .execute(&pool)
            .await
            .expect("the same chat may have a session per company");
    }
}
