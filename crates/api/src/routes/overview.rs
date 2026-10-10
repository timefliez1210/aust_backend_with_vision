//! `GET /api/v1/admin/overview` — everything the console's "Heute" screen shows, in
//! one round-trip.
//!
//! **Why one endpoint**: the screen is the first thing anyone sees on a phone in a
//! van; eight requests on a bad connection is eight chances to show half a page.
//!
//! **Rule**: no number here is computed a second way. Revenue and receivables come
//! from the Rechnungsausgangsbuch's own rows, offers from the KVA-Buch's judgement,
//! capacity from the calendar's schedule queries, result from the Gewinn service —
//! each tile links to the page whose number it repeats, and the two must agree.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{extract::State, routing::get, Extension, Json, Router};
use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, Utc, Weekday};
use serde::Serialize;
use uuid::Uuid;

use aust_core::models::TokenClaims;

use super::admin::{issued_register, kva_states, WON_INQUIRY_STATUSES};
use crate::repositories::{accounting_repo, admin_repo, calendar_repo, overview_repo};
use crate::services::{billing_reminder_service, profit_service};
use crate::{ApiError, AppState};

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/overview", get(overview))
}

/// Days of history the funnel and the acceptance rate look at.
const WINDOW_DAYS: u64 = 90;
/// Weeks of capacity shown, starting with the current week.
const CAPACITY_WEEKS: u64 = 4;
/// How far ahead an unstaffed job counts as "needs a crew now".
const UNSTAFFED_HORIZON_DAYS: u64 = 2;
/// How far ahead an overbooked day is flagged (same window as the old dashboard).
const OVERBOOK_HORIZON_DAYS: u64 = 30;

#[derive(Debug, Serialize)]
struct OverviewResponse {
    today: NaiveDate,
    attention: Attention,
    /// Won jobs today and tomorrow, in start order.
    jobs: Vec<Job>,
    /// Last 12 Leistungsmonate, oldest first, current month last.
    revenue: Vec<RevenueMonth>,
    pipeline: Pipeline,
    funnel: Funnel,
    /// Mon–Sat of the current and the next three weeks.
    capacity: Vec<CapacityDay>,
    receivables: Receivables,
}

/// Raw counts behind the to-do list. Wording lives in the frontend.
#[derive(Debug, Serialize)]
struct Attention {
    flash_contacts: i64,
    flash_contacts_oldest: Option<DateTime<Utc>>,
    unread_emails: i64,
    unread_emails_oldest: Option<DateTime<Utc>>,
    /// Up to three, longest-waiting first.
    unread_email_senders: Vec<String>,
    /// Untouched inquiries (`pending`) — the Anfragen badge.
    new_inquiries: i64,
    /// Everything before an offer exists (`pending`, `info_requested`, `estimated`).
    open_inquiries: i64,
    kva_followups: i64,
    kva_followups_netto_cents: i64,
    overdue_invoices: i64,
    overdue_cents: i64,
    oldest_overdue_days: Option<i64>,
    /// Won jobs in the next days with nobody assigned on that day.
    unstaffed: Vec<UnstaffedJob>,
    /// Days in the next 30 with more bookings than capacity.
    overbooked: Vec<CapacityDay>,
    invoice_reminders_due: i64,
    /// `None` for non-admins — the review list is an admin-only endpoint.
    review_requests_due: Option<i64>,
}

#[derive(Debug, Serialize)]
struct UnstaffedJob {
    inquiry_id: Uuid,
    date: NaiveDate,
    customer_name: Option<String>,
    volume_m3: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Job {
    inquiry_id: Uuid,
    date: NaiveDate,
    start_time: NaiveTime,
    customer_name: Option<String>,
    customer_phone: Option<String>,
    departure_address: Option<String>,
    arrival_address: Option<String>,
    volume_m3: Option<f64>,
    /// "Max M." style names of the crew on *this* day.
    crew: Vec<String>,
    day_number: i32,
    total_days: i32,
    status: String,
    service_type: Option<String>,
}

#[derive(Debug, Serialize)]
struct RevenueMonth {
    /// First day of the month.
    month: NaiveDate,
    /// Netto, by Leistungsmonat — what the register sums.
    revenue_cents: i64,
    /// Gewinn tab's "Ergebnis" for the month. `None` for non-admins.
    result_cents: Option<i64>,
}

#[derive(Debug, Serialize)]
struct Pipeline {
    /// Open KVAs whose move still lies ahead (the KVA-Buch's "live" open).
    open_count: i64,
    open_netto_cents: i64,
    /// Won ÷ decided for KVAs written in the last 90 days, 0–1.
    win_rate: Option<f64>,
    /// Same for the 90 days before that — the comparison.
    win_rate_previous: Option<f64>,
    avg_won_netto_cents: Option<i64>,
    /// Rolling 90-day win rate as of each of the last 12 month ends, for the sparkline.
    win_rate_trend: Vec<Option<f64>>,
}

/// Counts of inquiries created in the last 90 days that reached each stage.
#[derive(Debug, Serialize, Default)]
struct Funnel {
    inquiries: i64,
    estimated: i64,
    offered: i64,
    won: i64,
    invoiced: i64,
}

#[derive(Debug, Serialize, Clone)]
struct CapacityDay {
    date: NaiveDate,
    booked: i32,
    capacity: i32,
}

#[derive(Debug, Serialize, Default)]
struct Receivables {
    open_cents: i64,
    /// Not yet due (or no due date).
    current_cents: i64,
    overdue_1_30_cents: i64,
    overdue_31_60_cents: i64,
    overdue_60_plus_cents: i64,
    /// Most overdue first, at most five.
    overdue: Vec<OverdueInvoice>,
}

#[derive(Debug, Serialize)]
struct OverdueInvoice {
    invoice_number: String,
    inquiry_id: Option<Uuid>,
    customer_name: Option<String>,
    days_overdue: i64,
    open_cents: i64,
}

async fn overview(
    State(state): State<Arc<AppState>>,
    Extension(claims): Extension<TokenClaims>,
) -> Result<Json<OverviewResponse>, ApiError> {
    let db = &state.db;
    let is_admin = claims.role.is_admin();
    let today = profit_service::today_berlin();

    // ── Calendar: jobs, unstaffed, capacity, overbooked ──────────────────────
    let week_start = today - Days::new(today.weekday().num_days_from_monday() as u64);
    let capacity_end = week_start + Days::new(CAPACITY_WEEKS * 7 - 1);
    let horizon_end = today + Days::new(OVERBOOK_HORIZON_DAYS);
    let schedule_from = week_start.min(today);
    let schedule_to = capacity_end.max(horizon_end);

    let inquiry_rows = calendar_repo::fetch_schedule_inquiries(db, schedule_from, schedule_to).await?;
    let item_rows = calendar_repo::fetch_schedule_calendar_items(db, schedule_from, schedule_to).await?;
    let overrides: HashMap<NaiveDate, i32> = calendar_repo::fetch_capacity_overrides_range(db, schedule_from, schedule_to)
        .await?
        .into_iter()
        .collect();
    let default_capacity = state.config.calendar.default_capacity;

    // Booked = inquiries + Termine on the day — exactly `calendar::get_schedule`'s count.
    let mut booked: HashMap<NaiveDate, i32> = HashMap::new();
    for r in &inquiry_rows {
        *booked.entry(r.effective_date).or_default() += 1;
    }
    for r in &item_rows {
        *booked.entry(r.effective_date).or_default() += 1;
    }
    let day = |d: NaiveDate| CapacityDay {
        date: d,
        booked: booked.get(&d).copied().unwrap_or(0),
        capacity: overrides.get(&d).copied().unwrap_or(default_capacity),
    };

    let capacity: Vec<CapacityDay> = (0..CAPACITY_WEEKS * 7)
        .map(|i| week_start + Days::new(i))
        .filter(|d| d.weekday() != Weekday::Sun)
        .map(day)
        .collect();
    let overbooked: Vec<CapacityDay> = (0..=OVERBOOK_HORIZON_DAYS)
        .map(|i| day(today + Days::new(i)))
        .filter(|d| d.booked > d.capacity)
        .collect();

    let tomorrow = today + Days::new(1);
    let is_won = |s: &str| WON_INQUIRY_STATUSES.contains(&s);
    let mut jobs: Vec<Job> = inquiry_rows
        .iter()
        .filter(|r| (r.effective_date == today || r.effective_date == tomorrow) && is_won(&r.status))
        .map(|r| Job {
            inquiry_id: r.inquiry_id,
            date: r.effective_date,
            start_time: r.start_time,
            customer_name: r.customer_name.clone(),
            customer_phone: r.customer_phone.clone(),
            departure_address: r.departure_address.clone(),
            arrival_address: r.arrival_address.clone(),
            volume_m3: r.volume_m3,
            crew: r
                .employee_names
                .as_deref()
                .map(|s| s.split(", ").map(str::to_string).collect())
                .unwrap_or_default(),
            day_number: r.day_number,
            total_days: r.total_days,
            status: r.status.clone(),
            service_type: r.service_type.clone(),
        })
        .collect();
    jobs.sort_by_key(|j| (j.date, j.start_time));

    let unstaffed_end = today + Days::new(UNSTAFFED_HORIZON_DAYS);
    let mut unstaffed: Vec<UnstaffedJob> = inquiry_rows
        .iter()
        .filter(|r| {
            r.effective_date >= today
                && r.effective_date <= unstaffed_end
                && r.employees_assigned == 0
                && is_won(&r.status)
        })
        .map(|r| UnstaffedJob {
            inquiry_id: r.inquiry_id,
            date: r.effective_date,
            customer_name: r.customer_name.clone(),
            volume_m3: r.volume_m3,
        })
        .collect();
    unstaffed.sort_by_key(|u| u.date);

    // ── Revenue (register) + result (Gewinn) ─────────────────────────────────
    let current_month = profit_service::month_start(today);
    let first_month = current_month - Months::new(11);
    let months: Vec<NaiveDate> = (0..12).map(|i| first_month + Months::new(i)).collect();

    let register = issued_register(db).await?;
    let mut revenue_by_month: HashMap<NaiveDate, i64> = HashMap::new();
    for e in &register.revenue {
        if let Some(d) = e.service_date {
            *revenue_by_month.entry(profit_service::month_start(d)).or_default() += e.netto_cents;
        }
    }

    let mut result_by_month: HashMap<NaiveDate, i64> = HashMap::new();
    if is_admin {
        let mut years = vec![first_month.year()];
        if current_month.year() != first_month.year() {
            years.push(current_month.year());
        }
        // Same Dauerauftrag drafts the Gewinn tab books (idempotent insert), once per
        // request, so "Ergebnis" here matches the Gewinn tab for the current month.
        accounting_repo::generate_recurring_drafts(db, current_month).await?;
        for y in years {
            for m in profit_service::overview_from(db, y, &register.revenue).await?.months {
                result_by_month.insert(m.month, m.result_cents);
            }
        }
    }

    let revenue: Vec<RevenueMonth> = months
        .iter()
        .map(|m| RevenueMonth {
            month: *m,
            revenue_cents: revenue_by_month.get(m).copied().unwrap_or(0),
            result_cents: if is_admin { Some(result_by_month.get(m).copied().unwrap_or(0)) } else { None },
        })
        .collect();

    // ── Offers (KVA-Buch) ────────────────────────────────────────────────────
    let kvas = kva_states(db, today).await?;
    let live_open: Vec<_> = kvas.iter().filter(|k| k.lage == "offen" && !k.move_date_passed).collect();
    let followups: Vec<_> = kvas.iter().filter(|k| k.needs_followup).collect();

    let window_start = today - Days::new(WINDOW_DAYS);
    let prev_start = window_start - Days::new(WINDOW_DAYS);
    let win_rate_between = |from: NaiveDate, to: NaiveDate| {
        let (won, lost) = kvas
            .iter()
            .filter(|k| k.kva_date >= from && k.kva_date < to)
            .fold((0i64, 0i64), |(w, l), k| match k.lage {
                "gewonnen" => (w + 1, l),
                "verloren" => (w, l + 1),
                _ => (w, l),
            });
        (won + lost > 0).then(|| won as f64 / (won + lost) as f64)
    };
    let won_recent: Vec<_> = kvas
        .iter()
        .filter(|k| k.lage == "gewonnen" && k.kva_date >= window_start)
        .collect();

    let pipeline = Pipeline {
        open_count: live_open.len() as i64,
        open_netto_cents: live_open.iter().map(|k| k.netto_cents).sum(),
        win_rate: win_rate_between(window_start, tomorrow),
        win_rate_previous: win_rate_between(prev_start, window_start),
        avg_won_netto_cents: (!won_recent.is_empty())
            .then(|| won_recent.iter().map(|k| k.netto_cents).sum::<i64>() / won_recent.len() as i64),
        // Rolling 90-day rate as of each of the last 12 month ends.
        win_rate_trend: months
            .iter()
            .map(|m| {
                let end = (*m + Months::new(1)).min(tomorrow);
                win_rate_between(end - Days::new(WINDOW_DAYS), end)
            })
            .collect(),
    };

    // ── Funnel ───────────────────────────────────────────────────────────────
    let mut funnel = Funnel::default();
    for r in overview_repo::funnel_since(db, window_start).await? {
        funnel.inquiries += 1;
        let won = is_won(&r.status);
        if r.has_volume || r.has_offer || won {
            funnel.estimated += 1;
        }
        if r.has_offer || won {
            funnel.offered += 1;
        }
        if won {
            funnel.won += 1;
        }
        if r.has_issued_invoice {
            funnel.invoiced += 1;
        }
    }

    // ── Receivables (register) ───────────────────────────────────────────────
    let mut receivables = Receivables::default();
    let mut overdue: Vec<OverdueInvoice> = Vec::new();
    for r in register.receivables {
        receivables.open_cents += r.open_cents;
        let days = r.due_date.map(|d| (today - d).num_days()).unwrap_or(0);
        match days {
            d if d <= 0 => receivables.current_cents += r.open_cents,
            1..=30 => receivables.overdue_1_30_cents += r.open_cents,
            31..=60 => receivables.overdue_31_60_cents += r.open_cents,
            _ => receivables.overdue_60_plus_cents += r.open_cents,
        }
        if days > 0 {
            overdue.push(OverdueInvoice {
                invoice_number: r.invoice_number,
                inquiry_id: r.inquiry_id,
                customer_name: r.customer_name,
                days_overdue: days,
                open_cents: r.open_cents,
            });
        }
    }
    overdue.sort_by(|a, b| b.days_overdue.cmp(&a.days_overdue));
    let overdue_invoices = overdue.len() as i64;
    let overdue_cents = overdue.iter().map(|o| o.open_cents).sum();
    let oldest_overdue_days = overdue.first().map(|o| o.days_overdue);
    overdue.truncate(5);
    receivables.overdue = overdue;

    // ── Attention ────────────────────────────────────────────────────────────
    let (unread_emails, _, _) = admin_repo::email_unread_counts(db).await?;
    let mail = overview_repo::unread_mail_summary(db).await?;
    let review_requests_due = if is_admin {
        Some(billing_reminder_service::list_due_review_requests(db).await?.len() as i64)
    } else {
        None
    };

    let attention = Attention {
        flash_contacts: admin_repo::count_open_flash_contacts(db).await?,
        flash_contacts_oldest: overview_repo::oldest_open_flash_contact(db).await?,
        unread_emails,
        unread_emails_oldest: mail.oldest,
        unread_email_senders: mail.senders,
        new_inquiries: admin_repo::count_new_inquiries(db).await?,
        open_inquiries: admin_repo::count_open_inquiries(db).await?,
        kva_followups: followups.len() as i64,
        kva_followups_netto_cents: followups.iter().map(|k| k.netto_cents).sum(),
        overdue_invoices,
        overdue_cents,
        oldest_overdue_days,
        unstaffed,
        overbooked,
        invoice_reminders_due: billing_reminder_service::list_due_invoice_reminders(db).await?.len() as i64,
        review_requests_due,
    };

    Ok(Json(OverviewResponse {
        today,
        attention,
        jobs,
        revenue,
        pipeline,
        funnel,
        capacity,
        receivables,
    }))
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::{
        generate_test_jwt, insert_test_employee, insert_test_inquiry_employee, insert_test_quote_with_status,
        test_app_state, update_inquiry_scheduled_date,
    };
    use axum::body::Body;
    use hyper::Request;
    use tower::ServiceExt;

    async fn fetch_overview() -> serde_json::Value {
        let app = crate::create_router(test_app_state().await);
        let resp = app
            .oneshot(
                Request::get("/api/v1/admin/overview")
                    .header("Authorization", format!("Bearer {}", generate_test_jwt()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn jobs_for(json: &serde_json::Value, id: uuid::Uuid) -> Vec<serde_json::Value> {
        json["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["inquiry_id"] == id.to_string())
            .cloned()
            .collect()
    }

    fn is_unstaffed(json: &serde_json::Value, id: uuid::Uuid) -> bool {
        json["attention"]["unstaffed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["inquiry_id"] == id.to_string())
    }

    /// The fixed shape the screen relies on: 12 months ending with this one, and
    /// Mon–Sat of four weeks starting this Monday.
    #[tokio::test]
    async fn the_overview_covers_a_year_of_revenue_and_four_weeks_of_capacity() {
        let json = fetch_overview().await;
        let today = crate::services::profit_service::today_berlin();

        let revenue = json["revenue"].as_array().unwrap();
        assert_eq!(revenue.len(), 12);
        assert_eq!(
            revenue.last().unwrap()["month"],
            crate::services::profit_service::month_start(today).to_string()
        );
        // The test token is an admin, so the Gewinn result rides along.
        assert!(revenue[0]["result_cents"].is_i64());

        let capacity = json["capacity"].as_array().unwrap();
        assert_eq!(capacity.len(), 24);
        let first: chrono::NaiveDate = capacity[0]["date"].as_str().unwrap().parse().unwrap();
        assert_eq!(chrono::Datelike::weekday(&first), chrono::Weekday::Mon);
        assert!(first <= today && today - first < chrono::Duration::days(7));
    }

    /// A booked move today with nobody on it shows up as a job AND as "needs a crew";
    /// assigning someone keeps the job but clears the warning. A pending inquiry that
    /// merely carries a wish date is not a job.
    #[tokio::test]
    async fn a_won_job_without_crew_is_flagged_until_someone_is_assigned() {
        let state = test_app_state().await;
        let pool = state.db.clone();
        let today = crate::services::profit_service::today_berlin();

        let won = insert_test_quote_with_status(&pool, "scheduled").await;
        update_inquiry_scheduled_date(&pool, won, today, None).await.unwrap();
        let wish = insert_test_quote_with_status(&pool, "pending").await;
        update_inquiry_scheduled_date(&pool, wish, today, None).await.unwrap();

        let json = fetch_overview().await;
        assert_eq!(jobs_for(&json, won).len(), 1);
        assert!(jobs_for(&json, wish).is_empty());
        assert!(is_unstaffed(&json, won));

        // Unique surname: the test DB is shared and employees.email is unique.
        let surname = format!("M{}", uuid::Uuid::now_v7().simple());
        let employee = insert_test_employee(&pool, "Max", &surname).await;
        insert_test_inquiry_employee(&pool, won, employee, today, 8.0).await;

        let json = fetch_overview().await;
        let job = &jobs_for(&json, won)[0];
        assert_eq!(job["crew"][0], "Max M.");
        assert!(!is_unstaffed(&json, won));
    }
}
