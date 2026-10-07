//! Gewinn tab — revenue, labor and costs combined into margins.
//!
//! Alex's personal controlling (not tax bookkeeping). Revenue is netto by
//! Leistungsmonat, exactly as the Rechnungsausgangsbuch counts it.
//!
//! **Labor cost is extrapolated.** Workers are paid purely by the hour, so labor
//! cost = paid hours × rate. The rate starts at the default (€18.50 all-in) and is
//! replaced by each employee's *real* rate as Alex uses the backend as intended:
//! transfer the month's hours ("Stunden übernehmen") and book the wages against
//! the employee. Real rate = booked wages ÷ transferred hours over the last
//! [`RATE_WINDOW_MONTHS`] months that have both. Wages booked without an employee
//! (e.g. one SV transfer to the Krankenkasse) are spread over that month's crew by
//! hours. The gap between real rate and default is paid time that never reached a
//! job: sick days, Urlaub, yard work.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, Months, NaiveDate};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::repositories::accounting_repo::{
    self, BulkAdjustmentRow, CrewDayRow, LaborMonthInput, LaborMonthRow, WagesRow,
};
use crate::repositories::employee_repo::HoursAdjustmentRow;
use crate::routes::admin::{issued_revenue, paid_hours_for, RevenueEntry};
use crate::ApiError;

/// How many qualifying months feed the real hourly rate.
pub(crate) const RATE_WINDOW_MONTHS: usize = 6;

// ── Month helpers ───────────────────────────────────────────────────────────

pub(crate) fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).expect("day 1 exists")
}

pub(crate) fn month_end(m: NaiveDate) -> NaiveDate {
    (month_start(m) + Months::new(1)).pred_opt().expect("valid date")
}

pub(crate) fn parse_month(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d").ok()
}

pub(crate) fn today_berlin() -> NaiveDate {
    chrono::Utc::now().with_timezone(&chrono_tz::Europe::Berlin).date_naive()
}

fn add_months(m: NaiveDate, n: i32) -> NaiveDate {
    if n >= 0 {
        m + Months::new(n as u32)
    } else {
        m - Months::new((-n) as u32)
    }
}

fn cents(hours: f64, rate_cents: i64) -> i64 {
    (hours * rate_cents as f64).round() as i64
}

fn round2(h: f64) -> f64 {
    (h * 100.0).round() / 100.0
}

// ── Rates ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct RealRate {
    pub rate_cents: i64,
    /// Months that went into the rate.
    pub months: usize,
    pub hours: f64,
    pub wages_cents: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Rates {
    pub default_cents: i64,
    pub company: Option<RealRate>,
    pub per_employee: HashMap<Uuid, RealRate>,
}

/// Where an employee's rate comes from — shown next to every labor figure.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RateSource {
    /// The employee's own booked wages ÷ transferred hours.
    Belegt,
    /// No own history yet — the company-wide real rate.
    Betrieb,
    /// No data at all — the default (€18.50).
    Standard,
}

impl Rates {
    pub(crate) fn for_employee(&self, id: Uuid) -> (i64, RateSource) {
        if let Some(r) = self.per_employee.get(&id) {
            (r.rate_cents, RateSource::Belegt)
        } else {
            self.company_rate()
        }
    }

    pub(crate) fn company_rate(&self) -> (i64, RateSource) {
        match &self.company {
            Some(r) => (r.rate_cents, RateSource::Betrieb),
            None => (self.default_cents, RateSource::Standard),
        }
    }
}

/// Wages per employee for one month, with unlinked wages spread by hours share.
/// Returns (per-employee wages, total wages, total hours).
fn month_wages(
    hours: &HashMap<Uuid, f64>,
    wages: &[&WagesRow],
) -> (HashMap<Uuid, f64>, f64, f64) {
    let total_hours: f64 = hours.values().sum();
    let unlinked: f64 = wages.iter().filter(|w| w.employee_id.is_none()).map(|w| w.netto_cents as f64).sum();
    let mut per: HashMap<Uuid, f64> = HashMap::new();
    for w in wages.iter().filter_map(|w| w.employee_id.map(|e| (e, w.netto_cents))) {
        *per.entry(w.0).or_default() += w.1 as f64;
    }
    if total_hours > 0.0 && unlinked != 0.0 {
        for (e, h) in hours {
            *per.entry(*e).or_default() += unlinked * h / total_hours;
        }
    }
    let total: f64 = wages.iter().map(|w| w.netto_cents as f64).sum();
    (per, total, total_hours)
}

/// Derive real hourly rates from transferred hours and booked wages. Pure.
pub(crate) fn compute_rates(labor: &[LaborMonthRow], wages: &[WagesRow], default_cents: i64) -> Rates {
    let mut hours_by_month: BTreeMap<NaiveDate, HashMap<Uuid, f64>> = BTreeMap::new();
    for l in labor {
        *hours_by_month.entry(l.month).or_default().entry(l.employee_id).or_default() += l.paid_hours;
    }
    let mut wages_by_month: HashMap<NaiveDate, Vec<&WagesRow>> = HashMap::new();
    for w in wages {
        wages_by_month.entry(w.period_month).or_default().push(w);
    }

    let mut company = (0usize, 0.0f64, 0.0f64); // months, hours, wages
    let mut per: HashMap<Uuid, (usize, f64, f64)> = HashMap::new();

    // Newest first; each series stops after RATE_WINDOW_MONTHS qualifying months.
    // Only employees whose wages are booked count — a month where Alex has booked one
    // worker's Lohn so far must not divide that one wage by the whole crew's hours.
    for (month, hours) in hours_by_month.iter().rev() {
        let Some(ws) = wages_by_month.get(month) else { continue };
        let (per_emp, _, _) = month_wages(hours, ws);
        let (mut month_hours, mut month_wages_sum) = (0.0, 0.0);
        for (e, h) in hours {
            let w = per_emp.get(e).copied().unwrap_or(0.0);
            if *h <= 0.0 || w <= 0.0 {
                continue;
            }
            month_hours += h;
            month_wages_sum += w;
            let acc = per.entry(*e).or_default();
            if acc.0 < RATE_WINDOW_MONTHS {
                *acc = (acc.0 + 1, acc.1 + h, acc.2 + w);
            }
        }
        if month_hours > 0.0 && company.0 < RATE_WINDOW_MONTHS {
            company = (company.0 + 1, company.1 + month_hours, company.2 + month_wages_sum);
        }
    }

    let to_rate = |(months, hours, wages): (usize, f64, f64)| -> Option<RealRate> {
        (months > 0 && hours > 0.0).then(|| RealRate {
            rate_cents: (wages / hours).round() as i64,
            months,
            hours: round2(hours),
            wages_cents: wages.round() as i64,
        })
    };

    Rates {
        default_cents,
        company: to_rate(company),
        per_employee: per.into_iter().filter_map(|(e, acc)| to_rate(acc).map(|r| (e, r))).collect(),
    }
}

/// Rates from the last 24 months of data.
pub(crate) async fn load_rates(pool: &PgPool) -> Result<Rates, ApiError> {
    let to = month_start(today_berlin());
    let from = add_months(to, -24);
    let labor = accounting_repo::list_labor_months(pool, from, to).await?;
    let wages = accounting_repo::wages_by_month(pool, from, to).await?;
    let default = accounting_repo::get_default_rate(pool).await?;
    Ok(compute_rates(&labor, &wages, default))
}

// ── Paid hours (live, from the hours tab's data) ────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DayState {
    /// Clock times confirmed in the hours tab.
    Confirmed,
    /// Day is past but clock times are missing.
    Unconfirmed,
    /// Day is in the future — hours are the plan.
    Planned,
    /// Switched off in the payroll edit mode.
    Deactivated,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PaidDay {
    pub employee_id: Uuid,
    pub kind: String,
    pub source_id: Uuid,
    pub inquiry_id: Option<Uuid>,
    pub day: NaiveDate,
    /// What the hours tab pays for this day (0 when nothing is recorded yet).
    pub paid_hours: f64,
    pub worked_hours: f64,
    /// Best estimate: paid hours when known, else the planned hours.
    pub projected_hours: f64,
    pub state: DayState,
}

/// Apply the payroll override layer to crew days, the same way the hours tab does
/// (`admin::paid_hours_for`). Zusatztermine have no override layer yet.
pub(crate) fn paid_days(crew: &[CrewDayRow], adj: &[BulkAdjustmentRow], today: NaiveDate) -> Vec<PaidDay> {
    let mut map: HashMap<(Uuid, &str, Uuid, NaiveDate), HoursAdjustmentRow> = HashMap::new();
    for a in adj {
        let source = match a.entry_type.as_str() {
            "inquiry" => a.inquiry_id,
            "calendar_item" => a.calendar_item_id,
            _ => None,
        };
        if let Some(src) = source {
            map.insert(
                (a.employee_id, a.entry_type.as_str(), src, a.job_date),
                HoursAdjustmentRow {
                    entry_type: a.entry_type.clone(),
                    inquiry_id: a.inquiry_id,
                    calendar_item_id: a.calendar_item_id,
                    job_date: a.job_date,
                    deactivated: a.deactivated,
                    paid_clock_in: a.paid_clock_in,
                    paid_clock_out: a.paid_clock_out,
                    paid_break_minutes: a.paid_break_minutes,
                },
            );
        }
    }

    crew.iter()
        .map(|c| {
            let a = if c.kind == "appointment" {
                None
            } else {
                map.get(&(c.employee_id, c.kind.as_str(), c.source_id, c.day))
            };
            let paid = paid_hours_for(c.actual_hours, a);
            let deactivated = a.is_some_and(|a| a.deactivated);
            let state = if deactivated {
                DayState::Deactivated
            } else if c.confirmed || c.actual_hours.is_some() {
                DayState::Confirmed
            } else if c.day > today {
                DayState::Planned
            } else {
                DayState::Unconfirmed
            };
            let projected = match (paid, state) {
                (_, DayState::Deactivated) => 0.0,
                (Some(p), _) => p,
                (None, _) => c.planned_hours.unwrap_or(0.0),
            };
            PaidDay {
                employee_id: c.employee_id,
                kind: c.kind.clone(),
                source_id: c.source_id,
                inquiry_id: c.inquiry_id,
                day: c.day,
                paid_hours: paid.unwrap_or(0.0),
                worked_hours: c.actual_hours.unwrap_or(0.0),
                projected_hours: projected,
                state,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct EmployeeMonthHours {
    pub paid_hours: f64,
    pub worked_hours: f64,
    pub projected_hours: f64,
    pub unconfirmed_days: i32,
    pub planned_days: i32,
}

fn sum_by_employee(days: &[PaidDay]) -> HashMap<Uuid, EmployeeMonthHours> {
    let mut out: HashMap<Uuid, EmployeeMonthHours> = HashMap::new();
    for d in days {
        let e = out.entry(d.employee_id).or_default();
        e.paid_hours += d.paid_hours;
        e.worked_hours += d.worked_hours;
        e.projected_hours += d.projected_hours;
        match d.state {
            DayState::Unconfirmed => e.unconfirmed_days += 1,
            DayState::Planned => e.planned_days += 1,
            _ => {}
        }
    }
    out
}

async fn live_days(pool: &PgPool, from: NaiveDate, to: NaiveDate) -> Result<Vec<PaidDay>, ApiError> {
    let crew = accounting_repo::crew_days(pool, from, to).await?;
    let adj = accounting_repo::adjustments(pool, from, to).await?;
    Ok(paid_days(&crew, &adj, today_berlin()))
}

// ── Overview ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LaborSource {
    /// Wages booked for the month — actual money.
    Gebucht,
    /// Hours transferred, costed at the rate of the transfer.
    Uebernommen,
    /// Live hours from the hours tab (incl. planned crews) × current rate.
    Vorlaeufig,
    /// Nothing at all.
    Keine,
}

#[derive(Debug, Serialize)]
pub(crate) struct MonthRow {
    pub month: NaiveDate,
    pub revenue_cents: i64,
    pub labor_cents: i64,
    pub labor_source: LaborSource,
    pub labor_hours: f64,
    pub fixed_cents: i64,
    pub variable_cents: i64,
    /// Part of fixed + variable that is still an unconfirmed Dauerauftrag draft.
    pub draft_cents: i64,
    pub result_cents: i64,
    pub is_future: bool,
    pub is_current: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct VehicleCost {
    pub vehicle_id: Uuid,
    pub label: String,
    pub kennzeichen: String,
    pub total_cents: i64,
    pub per_month_cents: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct BreakEven {
    /// Netto revenue needed per month to cover fixed costs.
    pub revenue_cents: i64,
    pub fixed_cents: i64,
    /// Share of revenue left after labor and variable costs (0–1).
    pub contribution_ratio: f64,
    pub months: Vec<NaiveDate>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Overview {
    pub year: i32,
    pub months: Vec<MonthRow>,
    pub totals: MonthTotals,
    pub break_even: Option<BreakEven>,
    pub vehicles: Vec<VehicleCost>,
    pub open_drafts: i64,
    pub rates: RatesSummary,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct MonthTotals {
    pub revenue_cents: i64,
    pub labor_cents: i64,
    pub fixed_cents: i64,
    pub variable_cents: i64,
    pub result_cents: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct RatesSummary {
    pub default_cents: i64,
    pub company: Option<RealRate>,
    pub company_rate_cents: i64,
    pub company_source: RateSource,
}

fn rates_summary(r: &Rates) -> RatesSummary {
    let (rate, src) = r.company_rate();
    RatesSummary {
        default_cents: r.default_cents,
        company: r.company.clone(),
        company_rate_cents: rate,
        company_source: src,
    }
}

pub(crate) async fn overview(pool: &PgPool, year: i32) -> Result<Overview, ApiError> {
    accounting_repo::generate_recurring_drafts(pool, month_start(today_berlin())).await?;
    let revenue = issued_revenue(pool).await?;
    overview_from(pool, year, &revenue).await
}

/// The year's month rows from an already-loaded register, without generating drafts.
///
/// **Caller**: `overview`, `routes::overview` (Heute, which spans two years and
/// already holds the register)
/// **Why**: the dashboard must not reload the register or re-run draft generation
/// once per year it shows.
pub(crate) async fn overview_from(pool: &PgPool, year: i32, revenue: &[RevenueEntry]) -> Result<Overview, ApiError> {
    let today = today_berlin();
    let current = month_start(today);
    let first = NaiveDate::from_ymd_opt(year, 1, 1).ok_or_else(|| ApiError::BadRequest("Ungültiges Jahr".into()))?;
    let last = NaiveDate::from_ymd_opt(year, 12, 1).expect("valid");

    let rates = load_rates(pool).await?;
    let costs = accounting_repo::cost_by_kind(pool, first, last).await?;
    let wages = accounting_repo::wages_by_month(pool, first, last).await?;
    let labor = accounting_repo::list_labor_months(pool, first, last).await?;
    let days = live_days(pool, first, month_end(last)).await?;
    let vehicles = accounting_repo::cost_by_vehicle(pool, first, last).await?;
    let open_drafts = accounting_repo::count_drafts(pool).await?;
    let recurring = accounting_repo::list_recurring(pool).await?;

    let mut rows = Vec::with_capacity(12);
    let mut totals = MonthTotals::default();
    for m in 0..12 {
        let month = add_months(first, m);
        let end = month_end(month);

        let revenue_cents: i64 = revenue
            .iter()
            .filter(|r| r.service_date.is_some_and(|d| d >= month && d <= end))
            .map(|r| r.netto_cents)
            .sum();

        let mut fixed = 0;
        let mut variable = 0;
        let mut draft = 0;
        // Future months have no drafts yet: project the active Daueraufträge.
        if month > current {
            for r in recurring.iter().filter(|r| recurring_due(r, month)) {
                match r.category_kind.as_str() {
                    "fixed" => fixed += r.netto_cents,
                    "variable" => variable += r.netto_cents,
                    _ => continue,
                }
                draft += r.netto_cents;
            }
        }
        for c in costs.iter().filter(|c| c.period_month == month) {
            match c.kind.as_str() {
                "fixed" => fixed += c.netto_cents,
                "variable" => variable += c.netto_cents,
                _ => continue, // wages → labor line
            }
            if c.status == "draft" {
                draft += c.netto_cents;
            }
        }

        let month_wage_rows: Vec<&WagesRow> = wages.iter().filter(|w| w.period_month == month).collect();
        let snapshot: Vec<&LaborMonthRow> = labor.iter().filter(|l| l.month == month).collect();
        let month_days: Vec<PaidDay> = days.iter().filter(|d| d.day >= month && d.day <= end).cloned().collect();
        let (labor_cents, labor_source, labor_hours) =
            month_labor(&snapshot, &sum_by_employee(&month_days), &month_wage_rows, &rates);

        let result = revenue_cents - labor_cents - fixed - variable;
        totals.revenue_cents += revenue_cents;
        totals.labor_cents += labor_cents;
        totals.fixed_cents += fixed;
        totals.variable_cents += variable;
        totals.result_cents += result;

        rows.push(MonthRow {
            month,
            revenue_cents,
            labor_cents,
            labor_source,
            labor_hours: round2(labor_hours),
            fixed_cents: fixed,
            variable_cents: variable,
            draft_cents: draft,
            result_cents: result,
            is_future: month > current,
            is_current: month == current,
        });
    }

    let break_even = compute_break_even(&rows, current);

    let mut by_vehicle: BTreeMap<Uuid, VehicleCost> = BTreeMap::new();
    for v in &vehicles {
        let e = by_vehicle.entry(v.vehicle_id).or_insert_with(|| VehicleCost {
            vehicle_id: v.vehicle_id,
            label: v.vehicle_label.clone(),
            kennzeichen: v.kennzeichen.clone(),
            total_cents: 0,
            per_month_cents: 0,
        });
        e.total_cents += v.netto_cents;
    }
    // Average over the months of the year that have started.
    let elapsed = if year < current.year() {
        12
    } else if year > current.year() {
        1
    } else {
        current.month() as i64
    };
    let mut vehicles: Vec<VehicleCost> = by_vehicle
        .into_values()
        .map(|mut v| {
            v.per_month_cents = v.total_cents / elapsed.max(1);
            v
        })
        .collect();
    vehicles.sort_by(|a, b| b.total_cents.cmp(&a.total_cents));

    Ok(Overview {
        year,
        months: rows,
        totals,
        break_even,
        vehicles,
        open_drafts,
        rates: rates_summary(&rates),
    })
}

/// One month's labor cost, employee by employee: booked wages where Alex has booked
/// them, otherwise hours × rate. Hours come from the transferred snapshot when the
/// month was transferred, else live from the hours tab (planned crews included).
/// Labelled `Gebucht` only once every employee with hours has a booked wage.
pub(crate) fn month_labor(
    snapshot: &[&LaborMonthRow],
    live: &HashMap<Uuid, EmployeeMonthHours>,
    wages: &[&WagesRow],
    rates: &Rates,
) -> (i64, LaborSource, f64) {
    let hours: HashMap<Uuid, f64> = if snapshot.is_empty() {
        live.iter().map(|(e, h)| (*e, h.projected_hours)).collect()
    } else {
        snapshot.iter().map(|l| (l.employee_id, l.paid_hours)).collect()
    };
    let snap_cost: HashMap<Uuid, i64> = snapshot.iter().map(|l| (l.employee_id, l.cost_cents)).collect();
    let (per_emp, total_wages, total_hours) = month_wages(&hours, wages);

    if hours.is_empty() || total_hours == 0.0 {
        return if total_wages != 0.0 {
            (total_wages.round() as i64, LaborSource::Gebucht, 0.0)
        } else {
            (0, LaborSource::Keine, 0.0)
        };
    }

    let mut cost = 0.0;
    let mut all_booked = true;
    for (e, h) in &hours {
        match per_emp.get(e).copied().filter(|w| *w > 0.0) {
            Some(w) => cost += w,
            None => {
                all_booked &= *h <= 0.0;
                cost += snap_cost.get(e).map(|c| *c as f64).unwrap_or_else(|| *h * rates.for_employee(*e).0 as f64);
            }
        }
    }
    // Wages booked for someone without hours this month still cost money.
    for (e, w) in &per_emp {
        if !hours.contains_key(e) {
            cost += w;
        }
    }
    let source = if all_booked {
        LaborSource::Gebucht
    } else if !snapshot.is_empty() {
        LaborSource::Uebernommen
    } else {
        LaborSource::Vorlaeufig
    };
    (cost.round() as i64, source, round2(total_hours))
}

/// Does a recurring template charge in `month`?
pub(crate) fn recurring_due(r: &accounting_repo::RecurringRow, month: NaiveDate) -> bool {
    if !r.active || month < r.start_month || r.end_month.is_some_and(|e| month > e) {
        return false;
    }
    let diff = (month.year() - r.start_month.year()) * 12 + month.month() as i32 - r.start_month.month() as i32;
    diff % r.interval_months.max(1) as i32 == 0
}

/// Break-even from the last three complete months with revenue:
/// fixed ÷ (1 − (labor + variable) ÷ revenue).
pub(crate) fn compute_break_even(rows: &[MonthRow], current: NaiveDate) -> Option<BreakEven> {
    let sample: Vec<&MonthRow> = rows
        .iter()
        .filter(|r| r.month < current && r.revenue_cents > 0)
        .rev()
        .take(3)
        .collect();
    if sample.is_empty() {
        return None;
    }
    let revenue: i64 = sample.iter().map(|r| r.revenue_cents).sum();
    let variable: i64 = sample.iter().map(|r| r.labor_cents + r.variable_cents).sum();
    let fixed: i64 = sample.iter().map(|r| r.fixed_cents).sum::<i64>() / sample.len() as i64;
    let ratio = 1.0 - variable as f64 / revenue as f64;
    if ratio <= 0.0 {
        return None;
    }
    Some(BreakEven {
        revenue_cents: (fixed as f64 / ratio).round() as i64,
        fixed_cents: fixed,
        contribution_ratio: (ratio * 1000.0).round() / 1000.0,
        months: sample.iter().rev().map(|r| r.month).collect(),
    })
}

// ── Per-job margin ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RevenueSource {
    /// Issued invoices.
    Rechnung,
    /// No invoice yet — the active KVA's netto.
    Angebot,
    Keine,
}

#[derive(Debug, Serialize)]
pub(crate) struct CrewLine {
    pub employee_id: Uuid,
    pub name: String,
    pub hours: f64,
    pub rate_cents: i64,
    pub rate_source: RateSource,
    pub cost_cents: i64,
    pub planned: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct JobMargin {
    pub inquiry_id: Uuid,
    pub customer_name: Option<String>,
    pub route: Option<String>,
    pub status: String,
    pub scheduled_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub revenue_cents: i64,
    pub revenue_source: RevenueSource,
    pub labor_cents: i64,
    pub labor_hours: f64,
    /// True when any crew hours are still the plan, not recorded.
    pub labor_planned: bool,
    pub direct_cents: i64,
    pub margin_cents: i64,
    pub margin_pct: Option<f64>,
    pub crew: Vec<CrewLine>,
}

#[derive(Debug, Serialize)]
pub(crate) struct JobsResponse {
    pub month: NaiveDate,
    pub jobs: Vec<JobMargin>,
    pub totals: JobTotals,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct JobTotals {
    pub revenue_cents: i64,
    pub labor_cents: i64,
    pub direct_cents: i64,
    pub margin_cents: i64,
}

/// Paid days for specific jobs, with transferred months taking their frozen hours.
async fn job_days(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<PaidDay>, ApiError> {
    let crew = accounting_repo::crew_days_for_inquiries(pool, ids).await?;
    let adj = accounting_repo::adjustments_for_inquiries(pool, ids).await?;
    let mut days = paid_days(&crew, &adj, today_berlin());
    let (Some(min), Some(max)) = (days.iter().map(|d| d.day).min(), days.iter().map(|d| d.day).max()) else {
        return Ok(days);
    };
    let snapshots = accounting_repo::list_labor_months(pool, month_start(min), month_start(max)).await?;
    let frozen: HashMap<(Uuid, NaiveDate), &LaborMonthRow> =
        snapshots.iter().map(|s| ((s.employee_id, s.month), s)).collect();
    for d in &mut days {
        if let Some(s) = frozen.get(&(d.employee_id, month_start(d.day))) {
            let hours = s
                .breakdown
                .as_array()
                .into_iter()
                .flatten()
                .find(|e| {
                    e["kind"] == d.kind.as_str()
                        && e["source_id"] == d.source_id.to_string()
                        && e["day"] == d.day.to_string()
                })
                .and_then(|e| e["hours"].as_f64())
                .unwrap_or(0.0);
            d.paid_hours = hours;
            d.projected_hours = hours;
            if d.state == DayState::Planned || d.state == DayState::Unconfirmed {
                d.state = DayState::Confirmed;
            }
        }
    }
    Ok(days)
}

async fn build_margins(
    pool: &PgPool,
    jobs: Vec<accounting_repo::JobRow>,
    rates: &Rates,
) -> Result<Vec<JobMargin>, ApiError> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = jobs.iter().map(|j| j.id).collect();
    let days = job_days(pool, &ids).await?;
    let revenue = issued_revenue(pool).await?;
    let direct: HashMap<Uuid, i64> = accounting_repo::cost_by_inquiry(pool, &ids)
        .await?
        .into_iter()
        .map(|r| (r.inquiry_id, r.netto_cents))
        .collect();
    let names: HashMap<Uuid, String> = accounting_repo::employee_names(pool)
        .await?
        .into_iter()
        .map(|e| (e.id, e.name))
        .collect();

    Ok(jobs
        .into_iter()
        .map(|j| {
            let invoiced: Vec<i64> = revenue
                .iter()
                .filter(|r| r.inquiry_id == Some(j.id))
                .map(|r| r.netto_cents)
                .collect();
            let (revenue_cents, revenue_source) = if !invoiced.is_empty() {
                (invoiced.iter().sum(), RevenueSource::Rechnung)
            } else if let Some(o) = j.offer_netto_cents {
                (o, RevenueSource::Angebot)
            } else {
                (0, RevenueSource::Keine)
            };

            let mut per_emp: BTreeMap<Uuid, (f64, bool)> = BTreeMap::new();
            for d in days.iter().filter(|d| d.inquiry_id == Some(j.id)) {
                let e = per_emp.entry(d.employee_id).or_default();
                e.0 += d.projected_hours;
                e.1 |= matches!(d.state, DayState::Planned | DayState::Unconfirmed) && d.paid_hours == 0.0;
            }
            let crew: Vec<CrewLine> = per_emp
                .into_iter()
                .map(|(e, (hours, planned))| {
                    let (rate, src) = rates.for_employee(e);
                    CrewLine {
                        employee_id: e,
                        name: names.get(&e).cloned().unwrap_or_else(|| "Unbekannt".into()),
                        hours: round2(hours),
                        rate_cents: rate,
                        rate_source: src,
                        cost_cents: cents(hours, rate),
                        planned,
                    }
                })
                .collect();
            let labor_cents: i64 = crew.iter().map(|c| c.cost_cents).sum();
            let labor_hours: f64 = crew.iter().map(|c| c.hours).sum();
            let direct_cents = direct.get(&j.id).copied().unwrap_or(0);
            let margin = revenue_cents - labor_cents - direct_cents;
            let route = match (&j.origin_city, &j.destination_city) {
                (Some(a), Some(b)) => Some(format!("{a} → {b}")),
                (Some(a), None) => Some(a.clone()),
                (None, Some(b)) => Some(b.clone()),
                _ => None,
            };
            JobMargin {
                inquiry_id: j.id,
                customer_name: j.customer_name,
                route,
                status: j.status,
                scheduled_date: j.scheduled_date,
                end_date: j.end_date,
                revenue_cents,
                revenue_source,
                labor_cents,
                labor_hours: round2(labor_hours),
                labor_planned: crew.iter().any(|c| c.planned),
                direct_cents,
                margin_cents: margin,
                margin_pct: (revenue_cents > 0)
                    .then(|| ((margin as f64 / revenue_cents as f64) * 1000.0).round() / 10.0),
                crew,
            }
        })
        .collect())
}

pub(crate) async fn jobs(pool: &PgPool, month: NaiveDate) -> Result<JobsResponse, ApiError> {
    let rates = load_rates(pool).await?;
    let jobs = accounting_repo::jobs_in_month(pool, month, month_end(month)).await?;
    let jobs = build_margins(pool, jobs, &rates).await?;
    let mut totals = JobTotals::default();
    for j in &jobs {
        totals.revenue_cents += j.revenue_cents;
        totals.labor_cents += j.labor_cents;
        totals.direct_cents += j.direct_cents;
        totals.margin_cents += j.margin_cents;
    }
    Ok(JobsResponse { month, jobs, totals })
}

/// Margin preview for one inquiry: the KVA estimate (persons × hours × company
/// rate) next to the actual figures once crew hours exist.
#[derive(Debug, Serialize)]
pub(crate) struct InquiryMargin {
    pub estimate: Option<MarginEstimate>,
    pub actual: Option<JobMargin>,
    pub rates: RatesSummary,
    /// Full-cost hourly rate (Vollkostensatz) for the KVA editor's warning.
    pub hourly: Option<HourlySummary>,
}

#[derive(Debug, Serialize)]
pub(crate) struct HourlySummary {
    pub break_even_cents: i64,
    pub target_rate_cents: i64,
    pub inaccurate: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct MarginEstimate {
    pub revenue_cents: i64,
    pub persons: i32,
    pub hours: f64,
    pub rate_cents: i64,
    pub rate_source: RateSource,
    pub labor_cents: i64,
    pub direct_cents: i64,
    pub margin_cents: i64,
    pub margin_pct: Option<f64>,
}

pub(crate) async fn inquiry_margin(
    pool: &PgPool,
    id: Uuid,
    current_rate_cents: i64,
) -> Result<InquiryMargin, ApiError> {
    let offer = accounting_repo::fetch_offer_for_preview(pool, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Anfrage nicht gefunden".into()))?;
    let rates = load_rates(pool).await?;
    let direct = accounting_repo::cost_by_inquiry(pool, &[id])
        .await?
        .first()
        .map(|r| r.netto_cents)
        .unwrap_or(0);

    let estimate = match offer {
        (Some(price), Some(persons), Some(hours)) if persons > 0 && hours > 0.0 => {
            let (rate, src) = rates.company_rate();
            let labor = cents(persons as f64 * hours, rate);
            let margin = price - labor - direct;
            Some(MarginEstimate {
                revenue_cents: price,
                persons,
                hours,
                rate_cents: rate,
                rate_source: src,
                labor_cents: labor,
                direct_cents: direct,
                margin_cents: margin,
                margin_pct: (price > 0).then(|| ((margin as f64 / price as f64) * 1000.0).round() / 10.0),
            })
        }
        _ => None,
    };

    let actual = match accounting_repo::fetch_job(pool, id).await? {
        Some(job) => {
            let m = build_margins(pool, vec![job], &rates).await?.into_iter().next();
            m.filter(|m| !m.crew.is_empty())
        }
        None => None,
    };

    let hourly = hourly_rate(pool, current_rate_cents).await?.map(|h| HourlySummary {
        break_even_cents: h.break_even_cents,
        target_rate_cents: h.target_rate_cents,
        inaccurate: h.inaccurate,
    });

    Ok(InquiryMargin { estimate, actual, rates: rates_summary(&rates), hourly })
}

// ── Employees: real cost per employee ───────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct EmployeeMonth {
    pub month: NaiveDate,
    pub hours: f64,
    /// Hours come from the transferred snapshot (true) or live (false).
    pub transferred: bool,
    /// Booked wages incl. the employee's share of unlinked wages.
    pub wages_cents: i64,
    pub rate_cents: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EmployeeCost {
    pub employee_id: Uuid,
    pub name: String,
    pub active: bool,
    pub rate_cents: i64,
    pub rate_source: RateSource,
    pub real: Option<RealRate>,
    pub months: Vec<EmployeeMonth>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EmployeesResponse {
    pub months: Vec<NaiveDate>,
    pub employees: Vec<EmployeeCost>,
    pub rates: RatesSummary,
}

pub(crate) async fn employees(pool: &PgPool) -> Result<EmployeesResponse, ApiError> {
    let current = month_start(today_berlin());
    let from = add_months(current, -5);
    let rates = load_rates(pool).await?;
    let labor = accounting_repo::list_labor_months(pool, from, current).await?;
    let wages = accounting_repo::wages_by_month(pool, from, current).await?;
    let days = live_days(pool, from, month_end(current)).await?;
    let names = accounting_repo::employee_names(pool).await?;
    let months: Vec<NaiveDate> = (0..6).map(|i| add_months(from, i)).collect();

    // Per month: hours per employee (snapshot wins over live) and their wage share.
    let mut month_hours: HashMap<NaiveDate, (HashMap<Uuid, f64>, bool)> = HashMap::new();
    let mut month_share: HashMap<NaiveDate, HashMap<Uuid, f64>> = HashMap::new();
    for m in &months {
        let snap: HashMap<Uuid, f64> = labor.iter().filter(|l| l.month == *m).map(|l| (l.employee_id, l.paid_hours)).collect();
        let (hours, transferred) = if snap.is_empty() {
            let end = month_end(*m);
            let live: Vec<PaidDay> = days.iter().filter(|d| d.day >= *m && d.day <= end).cloned().collect();
            (sum_by_employee(&live).into_iter().map(|(e, h)| (e, h.paid_hours)).collect(), false)
        } else {
            (snap, true)
        };
        let ws: Vec<&WagesRow> = wages.iter().filter(|w| w.period_month == *m).collect();
        let (share, _, _) = month_wages(&hours, &ws);
        month_share.insert(*m, share);
        month_hours.insert(*m, (hours, transferred));
    }

    let mut employees = Vec::new();
    for n in names {
        let mut row_months = Vec::new();
        let mut warnings = Vec::new();
        let mut any = false;
        for m in &months {
            let (hours_map, transferred) = &month_hours[m];
            let hours = hours_map.get(&n.id).copied().unwrap_or(0.0);
            let wages_cents = month_share[m].get(&n.id).copied().unwrap_or(0.0).round() as i64;
            any |= hours > 0.0 || wages_cents != 0;
            let label = format!("{:02}/{}", m.month(), m.year());
            if wages_cents > 0 && hours == 0.0 {
                warnings.push(format!("{label}: Lohn gebucht, aber keine Stunden erfasst"));
            }
            row_months.push(EmployeeMonth {
                month: *m,
                hours: round2(hours),
                transferred: *transferred,
                wages_cents,
                rate_cents: (hours > 0.0 && wages_cents > 0).then(|| (wages_cents as f64 / hours).round() as i64),
            });
        }
        if !any && !n.active {
            continue;
        }
        let (rate, src) = rates.for_employee(n.id);
        let real = rates.per_employee.get(&n.id).cloned();
        if let Some(r) = &real {
            // A real rate far above the default means paid time that never reaches
            // the hours tab — the gap Alex wants to see.
            if r.rate_cents as f64 > rates.default_cents as f64 * 1.25 {
                let gap = 1.0 - rates.default_cents as f64 / r.rate_cents as f64;
                warnings.push(format!(
                    "Echter Satz {:.2} € liegt {:.0} % über dem Standard — ca. {:.0} % der bezahlten Zeit ist keinem Einsatz zugeordnet",
                    r.rate_cents as f64 / 100.0,
                    (r.rate_cents as f64 / rates.default_cents as f64 - 1.0) * 100.0,
                    gap * 100.0
                ));
            }
        }
        employees.push(EmployeeCost {
            employee_id: n.id,
            name: n.name,
            active: n.active,
            rate_cents: rate,
            rate_source: src,
            real,
            months: row_months,
            warnings,
        });
    }

    Ok(EmployeesResponse { months, employees, rates: rates_summary(&rates) })
}

// ── Stundensatz-Kalkulation (full-cost hourly rate) ─────────────────────────
//
// "How much must one sold crew hour cost the customer?" Workers are paid only for
// hours worked, so wages are a cost *per hour*; only rent, insurance, software,
// marketing, the lift loan … are fixed. The rate therefore has two parts:
//
//     rate(H) = w + v + F ÷ H
//
//     w = wage cost per sold hour   (all wages ÷ hours sold to customers — unbilled
//                                    paid time like yard work lands here)
//     v = variable cost per hour    (variable categories loaded onto the rate)
//     F = fixed costs per month     (fixed categories loaded onto the rate)
//     H = sold crew hours per month (measured average, or Alex's plan)
//
// With a target profit P per month the quote rate is w + v + (F + P) ÷ H, and at the
// current KVA rate R the hours needed are (F + P) ÷ (R − w − v).

/// One month of inputs to the hourly-rate calculation.
#[derive(Debug, Clone)]
pub(crate) struct RateMonth {
    pub month: NaiveDate,
    /// Crew hours on customer jobs (moves + Zusatztermine), paid.
    pub sold_hours: f64,
    pub wage_cents: i64,
    pub wages_booked: bool,
    /// (category name, kind, in_hourly_rate, netto cents)
    pub costs: Vec<(String, String, bool, i64)>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct RateComponent {
    pub label: String,
    /// `wages` | `variable` | `fixed`
    pub kind: String,
    pub per_month_cents: i64,
    /// Share of one sold hour at the basis volume.
    pub per_hour_cents: i64,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct VolumeRow {
    pub hours: f64,
    /// `low` | `average` | `high` | `plan` | `capacity`
    pub kind: String,
    pub fixed_share_cents: i64,
    pub break_even_cents: i64,
    pub with_profit_cents: i64,
    /// Monthly result at the current KVA rate and this volume (before target profit).
    pub result_at_current_cents: i64,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct HourlyRate {
    pub months: Vec<NaiveDate>,
    pub window_months: i32,
    /// Fewer than 12 months, wages partly estimated, or no fixed costs booked.
    pub inaccurate: bool,
    pub warnings: Vec<String>,
    pub wage_per_hour_cents: i64,
    pub variable_per_hour_cents: i64,
    pub fixed_per_month_cents: i64,
    pub excluded_per_month_cents: i64,
    pub target_profit_cents: i64,
    pub avg_sold_hours: f64,
    pub planned_hours: Option<f64>,
    /// The volume the headline figures use: plan if set, else the average.
    pub basis_hours: f64,
    pub capacity_hours: f64,
    pub capacity_crew: i64,
    pub break_even_cents: i64,
    pub target_rate_cents: i64,
    /// Break-even if the crew were fully booked — the comparison line.
    pub capacity_break_even_cents: i64,
    pub current_rate_cents: i64,
    /// Sold hours per month needed at the current rate (`None` = the rate doesn't
    /// even cover wages + variable costs).
    pub hours_needed_break_even: Option<f64>,
    pub hours_needed_target: Option<f64>,
    /// Expected monthly result at the current rate and basis volume.
    pub result_at_current_cents: i64,
    pub components: Vec<RateComponent>,
    pub volumes: Vec<VolumeRow>,
}

/// Pure calculation over the sample months. `None` until a month with sold hours exists.
pub(crate) fn compute_hourly_rate(
    months: &[RateMonth],
    settings: &accounting_repo::HourlyCalcSettings,
    current_rate_cents: i64,
    active_employees: i64,
) -> Option<HourlyRate> {
    // The sample starts at the first month with jobs: before that, hours simply
    // weren't tracked, and counting those months would load a year of rent onto a
    // handful of hours. From there on every month counts, quiet ones included.
    let first = months.iter().position(|m| m.sold_hours > 0.0)?;
    let sample = &months[first..];
    let n = sample.len() as f64;

    let hours: f64 = sample.iter().map(|m| m.sold_hours).sum();
    let wages: i64 = sample.iter().map(|m| m.wage_cents).sum();
    let mut fixed = 0i64;
    let mut variable = 0i64;
    let mut excluded = 0i64;
    let mut by_cat: BTreeMap<(String, String), i64> = BTreeMap::new();
    for m in sample {
        for (name, kind, in_rate, c) in &m.costs {
            if kind == "wages" {
                continue; // wages come from the labor model, not the category sums
            }
            if !in_rate {
                excluded += c;
                continue;
            }
            match kind.as_str() {
                "fixed" => fixed += c,
                _ => variable += c,
            }
            *by_cat.entry((kind.clone(), name.clone())).or_default() += c;
        }
    }

    let w = wages as f64 / hours;
    let v = variable as f64 / hours;
    let f = fixed as f64 / n;
    let p = settings.target_profit_cents as f64;
    let avg = hours / n;
    let planned = settings.planned_hours_per_month.filter(|h| *h > 0.0);
    let basis = planned.unwrap_or(avg);
    let crew = settings.capacity_crew.map(i64::from).unwrap_or(active_employees).max(0);
    let capacity = crew as f64 * settings.capacity_hours_per_day * settings.capacity_days_per_month;

    let rate_at = |h: f64, extra: f64| -> i64 { (w + v + (f + extra) / h.max(1.0)).round() as i64 };
    let r = current_rate_cents as f64;
    let margin = r - w - v;
    let result_at = |h: f64| -> i64 { (margin * h - f).round() as i64 };

    let mut volumes: Vec<VolumeRow> = Vec::new();
    let mut push = |h: f64, kind: &str| {
        let h = h.round();
        if h <= 0.0 || volumes.iter().any(|x| (x.hours - h).abs() < 1.0) {
            return;
        }
        volumes.push(VolumeRow {
            hours: h,
            kind: kind.into(),
            fixed_share_cents: (f / h).round() as i64,
            break_even_cents: rate_at(h, 0.0),
            with_profit_cents: rate_at(h, p),
            result_at_current_cents: result_at(h),
        });
    };
    if let Some(pl) = planned {
        push(pl, "plan");
    }
    push(avg, "average");
    push(avg * 2.0 / 3.0, "low");
    push(avg * 4.0 / 3.0, "high");
    if capacity > 0.0 {
        push(capacity, "capacity");
    }
    volumes.sort_by(|a, b| a.hours.total_cmp(&b.hours));

    let per_hour = |c: f64| (c / basis.max(1.0)).round() as i64;
    let mut components = vec![RateComponent {
        label: "Löhne".into(),
        kind: "wages".into(),
        per_month_cents: (wages as f64 / n).round() as i64,
        per_hour_cents: w.round() as i64,
    }];
    let mut cats: Vec<RateComponent> = by_cat
        .into_iter()
        .map(|((kind, name), c)| {
            let per_month = c as f64 / n;
            RateComponent {
                label: name,
                per_hour_cents: if kind == "fixed" { per_hour(per_month) } else { (c as f64 / hours).round() as i64 },
                per_month_cents: per_month.round() as i64,
                kind,
            }
        })
        .collect();
    cats.sort_by(|a, b| b.per_hour_cents.cmp(&a.per_hour_cents));
    components.extend(cats);

    let mut warnings = Vec::new();
    if sample.len() < 12 {
        warnings.push(format!(
            "Erst {} Monat{} Daten — Saisonschwankungen sind noch nicht abgedeckt.",
            sample.len(),
            if sample.len() == 1 { "" } else { "e" }
        ));
    }
    let estimated = sample.iter().filter(|m| !m.wages_booked).count();
    if estimated > 0 {
        warnings.push(format!(
            "Löhne in {estimated} von {} Monaten nur geschätzt (Stunden × Satz) — Löhne buchen macht w genauer.",
            sample.len()
        ));
    }
    if fixed == 0 {
        warnings.push("Noch keine Fixkosten gebucht — Miete, Versicherung usw. als Daueraufträge anlegen.".into());
    }

    let needed = |extra: f64| (margin > 0.0).then(|| ((f + extra) / margin * 10.0).round() / 10.0);

    Some(HourlyRate {
        months: sample.iter().map(|m| m.month).collect(),
        window_months: settings.window_months,
        inaccurate: !warnings.is_empty(),
        warnings,
        wage_per_hour_cents: w.round() as i64,
        variable_per_hour_cents: v.round() as i64,
        fixed_per_month_cents: f.round() as i64,
        excluded_per_month_cents: (excluded as f64 / n).round() as i64,
        target_profit_cents: settings.target_profit_cents,
        avg_sold_hours: round2(avg),
        planned_hours: planned,
        basis_hours: round2(basis),
        capacity_hours: round2(capacity),
        capacity_crew: crew,
        break_even_cents: rate_at(basis, 0.0),
        target_rate_cents: rate_at(basis, p),
        capacity_break_even_cents: if capacity > 0.0 { rate_at(capacity, 0.0) } else { 0 },
        current_rate_cents,
        hours_needed_break_even: needed(0.0),
        hours_needed_target: needed(p),
        result_at_current_cents: result_at(basis),
        components,
        volumes,
    })
}

/// Sold hours per month: frozen breakdown for transferred months, live otherwise.
/// Calendar items (Lager, Werkstatt …) are paid but never sold, so they're left out.
fn sold_hours(snapshot: &[&LaborMonthRow], live: &[PaidDay]) -> f64 {
    let is_sold = |k: &str| k == "inquiry" || k == "appointment";
    if snapshot.is_empty() {
        live.iter().filter(|d| is_sold(&d.kind)).map(|d| d.paid_hours).sum()
    } else {
        snapshot
            .iter()
            .flat_map(|s| s.breakdown.as_array().cloned().unwrap_or_default())
            .filter(|e| e["kind"].as_str().is_some_and(is_sold))
            .filter_map(|e| e["hours"].as_f64())
            .sum()
    }
}

pub(crate) async fn hourly_rate(pool: &PgPool, current_rate_cents: i64) -> Result<Option<HourlyRate>, ApiError> {
    let settings = accounting_repo::get_hourly_calc(pool).await?;
    let current = month_start(today_berlin());
    let to = add_months(current, -1);
    let from = add_months(current, -settings.window_months.clamp(1, 24));

    accounting_repo::generate_recurring_drafts(pool, current).await?;
    let rates = load_rates(pool).await?;
    let labor = accounting_repo::list_labor_months(pool, from, to).await?;
    let wages = accounting_repo::wages_by_month(pool, from, to).await?;
    let days = live_days(pool, from, month_end(to)).await?;
    let costs = accounting_repo::cost_by_category(pool, from, to).await?;
    let active = accounting_repo::count_active_employees(pool).await?;

    let mut months = Vec::new();
    let mut m = from;
    while m <= to {
        let end = month_end(m);
        let snapshot: Vec<&LaborMonthRow> = labor.iter().filter(|l| l.month == m).collect();
        let live: Vec<PaidDay> = days.iter().filter(|d| d.day >= m && d.day <= end).cloned().collect();
        let ws: Vec<&WagesRow> = wages.iter().filter(|w| w.period_month == m).collect();
        let (wage_cents, source, _) = month_labor(&snapshot, &sum_by_employee(&live), &ws, &rates);
        months.push(RateMonth {
            month: m,
            sold_hours: sold_hours(&snapshot, &live),
            wage_cents,
            wages_booked: source == LaborSource::Gebucht,
            costs: costs
                .iter()
                .filter(|c| c.period_month == m)
                .map(|c| (c.category_name.clone(), c.kind.clone(), c.in_hourly_rate, c.netto_cents))
                .collect(),
        });
        m = add_months(m, 1);
    }
    Ok(compute_hourly_rate(&months, &settings, current_rate_cents, active))
}

// ── Weiterberechnete Kosten (recharged costs) ───────────────────────────────
//
// A cost category can be paid for by customers through KVA/invoice positions
// (Kraftstoff → Fahrkostenpauschale, Kartons → Verkauf U-Karton …). Whether that is
// a pass-through, profitable or loss-making is computed by comparing what came in
// through those positions with what the category cost — never declared.

/// Normalised position name for matching: lowercase, single spaces.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Euro amount of one KVA line (`flat_total`, else quantity × unit_price), in cents.
fn offer_line_cents(li: &serde_json::Value) -> i64 {
    let euros = li["flat_total"]
        .as_f64()
        .unwrap_or_else(|| li["quantity"].as_f64().unwrap_or(0.0) * li["unit_price"].as_f64().unwrap_or(0.0));
    (euros * 100.0).round() as i64
}

/// Split one issued invoice's netto into (position name, cents). Labor lines are
/// skipped — they're the hourly rate, not a recharged cost. Pure.
///
/// - manual invoice: its own lines (Menge × Einzelpreis)
/// - from a KVA: the KVA's non-labor lines × the invoice's share of the job
///   (full 1, Anzahlung p %, Schlussrechnung 1 − p %), plus the invoice's extras
pub(crate) fn invoice_positions(inv: &accounting_repo::PositionInvoiceRow) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    if inv.is_manual {
        for li in inv.line_items_json.as_ref().and_then(|j| j.as_array()).into_iter().flatten() {
            let cents = (li["quantity"].as_f64().unwrap_or(0.0) * li["unit_price_cents"].as_f64().unwrap_or(0.0)).round() as i64;
            if let Some(d) = li["description"].as_str() {
                out.push((d.trim().to_string(), cents));
            }
        }
        return out;
    }
    let pct = inv.partial_percent.or(inv.deposit_percent.map(i32::from)).unwrap_or(0) as f64 / 100.0;
    let share = match inv.invoice_type.as_str() {
        "partial_first" => pct,
        "partial_final" => 1.0 - pct,
        _ => 1.0,
    };
    for li in inv.offer_line_items.as_ref().and_then(|j| j.as_array()).into_iter().flatten() {
        if li["is_labor"].as_bool().unwrap_or(false) {
            continue;
        }
        if let Some(d) = li["description"].as_str() {
            out.push((d.trim().to_string(), (offer_line_cents(li) as f64 * share).round() as i64));
        }
    }
    if inv.invoice_type != "partial_first" {
        for e in inv.extra_services.as_array().into_iter().flatten() {
            if let (Some(d), Some(c)) = (e["description"].as_str(), e["price_cents"].as_i64()) {
                out.push((d.trim().to_string(), c));
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RechargeVerdict {
    /// More came in than it cost.
    Gewinn,
    /// Within ±2 % (min. 5 €): a true pass-through.
    Durchlauf,
    Verlust,
    /// Costs booked but the positions brought nothing in (or vice versa) — usually
    /// purchase and sale in different months, or the position isn't on any invoice.
    Unklar,
}

#[derive(Debug, Serialize)]
pub(crate) struct RechargeMonth {
    pub month: NaiveDate,
    pub revenue_cents: i64,
    pub cost_cents: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct RechargeRow {
    pub category_id: Uuid,
    pub category_name: String,
    pub positions: Vec<String>,
    pub revenue_cents: i64,
    pub cost_cents: i64,
    pub result_cents: i64,
    pub verdict: RechargeVerdict,
    pub months: Vec<RechargeMonth>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RechargeReport {
    pub year: i32,
    pub rows: Vec<RechargeRow>,
    /// Invoices imported from the Excel book carry no positions.
    pub legacy_invoices: i64,
}

pub(crate) fn verdict(revenue: i64, cost: i64) -> RechargeVerdict {
    if revenue == 0 || cost == 0 {
        return if revenue == 0 && cost == 0 { RechargeVerdict::Durchlauf } else { RechargeVerdict::Unklar };
    }
    let result = revenue - cost;
    let tolerance = (revenue.abs().max(cost.abs()) as f64 * 0.02).max(500.0) as i64;
    if result.abs() <= tolerance {
        RechargeVerdict::Durchlauf
    } else if result > 0 {
        RechargeVerdict::Gewinn
    } else {
        RechargeVerdict::Verlust
    }
}

pub(crate) async fn recharge_report(pool: &PgPool, year: i32) -> Result<RechargeReport, ApiError> {
    let first = NaiveDate::from_ymd_opt(year, 1, 1).ok_or_else(|| ApiError::BadRequest("Ungültiges Jahr".into()))?;
    let last = NaiveDate::from_ymd_opt(year, 12, 1).expect("valid");
    accounting_repo::generate_recurring_drafts(pool, month_start(today_berlin())).await?;

    let categories = accounting_repo::list_categories(pool).await?;
    let invoices = accounting_repo::invoices_for_positions(pool, first, month_end(last)).await?;
    let costs = accounting_repo::cost_by_category(pool, first, last).await?;
    let legacy_invoices = accounting_repo::count_legacy_invoices(pool, first, month_end(last)).await?;

    // (month, normalised position) → revenue
    let mut by_pos: HashMap<(NaiveDate, String), i64> = HashMap::new();
    for inv in &invoices {
        let Some(date) = inv.service_date else { continue };
        for (name, cents) in invoice_positions(inv) {
            *by_pos.entry((month_start(date), norm(&name))).or_default() += cents;
        }
    }

    let mut rows = Vec::new();
    for c in categories.iter().filter(|c| !c.recharge_positions.is_empty()) {
        let wanted: Vec<String> = c.recharge_positions.iter().map(|p| norm(p)).collect();
        let mut months = Vec::new();
        for i in 0..12 {
            let m = add_months(first, i);
            let revenue: i64 = by_pos
                .iter()
                .filter(|((pm, name), _)| *pm == m && wanted.contains(name))
                .map(|(_, v)| *v)
                .sum();
            let cost: i64 = costs
                .iter()
                .filter(|x| x.period_month == m && x.category_name == c.name)
                .map(|x| x.netto_cents)
                .sum();
            months.push(RechargeMonth { month: m, revenue_cents: revenue, cost_cents: cost });
        }
        let revenue: i64 = months.iter().map(|m| m.revenue_cents).sum();
        let cost: i64 = months.iter().map(|m| m.cost_cents).sum();
        rows.push(RechargeRow {
            category_id: c.id,
            category_name: c.name.clone(),
            positions: c.recharge_positions.clone(),
            revenue_cents: revenue,
            cost_cents: cost,
            result_cents: revenue - cost,
            verdict: verdict(revenue, cost),
            months,
        });
    }
    Ok(RechargeReport { year, rows, legacy_invoices })
}

// ── Monthly hours transfer ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub(crate) struct TransferLine {
    pub employee_id: Uuid,
    pub name: String,
    pub paid_hours: f64,
    pub worked_hours: f64,
    pub unconfirmed_days: i32,
    pub planned_days: i32,
    pub rate_cents: i64,
    pub rate_source: RateSource,
    pub cost_cents: i64,
    /// The existing snapshot, if this month was transferred before.
    pub transferred_hours: Option<f64>,
    pub transferred_cost_cents: Option<i64>,
    pub changed: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransferPreview {
    pub month: NaiveDate,
    pub lines: Vec<TransferLine>,
    pub total_hours: f64,
    pub total_cost_cents: i64,
    pub transferred_at: Option<chrono::DateTime<chrono::Utc>>,
    pub has_changes: bool,
    pub month_complete: bool,
}

struct TransferData {
    preview: TransferPreview,
    inputs: Vec<LaborMonthInput>,
}

async fn transfer_data(pool: &PgPool, month: NaiveDate) -> Result<TransferData, ApiError> {
    let end = month_end(month);
    let days = live_days(pool, month, end).await?;
    let rates = load_rates(pool).await?;
    let existing = accounting_repo::list_labor_months(pool, month, month).await?;
    let names: HashMap<Uuid, String> = accounting_repo::employee_names(pool)
        .await?
        .into_iter()
        .map(|e| (e.id, e.name))
        .collect();
    let sums = sum_by_employee(&days);

    let mut ids: HashSet<Uuid> = sums.keys().copied().collect();
    ids.extend(existing.iter().map(|e| e.employee_id));

    let mut lines = Vec::new();
    let mut inputs = Vec::new();
    for id in ids {
        let s = sums.get(&id).cloned().unwrap_or_default();
        let old = existing.iter().find(|e| e.employee_id == id);
        // Re-transfer keeps the rate the month was first costed at; only the hours move.
        let (rate, src) = match old {
            Some(o) => (o.rate_cents as i64, rates.for_employee(id).1),
            None => rates.for_employee(id),
        };
        let paid = round2(s.paid_hours);
        let cost = cents(paid, rate);
        let changed = old.is_none_or(|o| (o.paid_hours - paid).abs() > 0.005);
        if paid > 0.0 || s.worked_hours > 0.0 {
            let breakdown: Vec<serde_json::Value> = days
                .iter()
                .filter(|d| d.employee_id == id && d.paid_hours > 0.0)
                .map(|d| {
                    serde_json::json!({
                        "kind": d.kind,
                        "source_id": d.source_id.to_string(),
                        "inquiry_id": d.inquiry_id,
                        "day": d.day.to_string(),
                        "hours": round2(d.paid_hours),
                    })
                })
                .collect();
            inputs.push(LaborMonthInput {
                employee_id: id,
                paid_hours: paid,
                worked_hours: round2(s.worked_hours),
                rate_cents: rate as i32,
                cost_cents: cost,
                unconfirmed_days: s.unconfirmed_days,
                breakdown: serde_json::Value::Array(breakdown),
            });
        }
        lines.push(TransferLine {
            employee_id: id,
            name: names.get(&id).cloned().unwrap_or_else(|| "Unbekannt".into()),
            paid_hours: paid,
            worked_hours: round2(s.worked_hours),
            unconfirmed_days: s.unconfirmed_days,
            planned_days: s.planned_days,
            rate_cents: rate,
            rate_source: src,
            cost_cents: cost,
            transferred_hours: old.map(|o| o.paid_hours),
            transferred_cost_cents: old.map(|o| o.cost_cents),
            changed,
        });
    }
    lines.sort_by(|a, b| a.name.cmp(&b.name));

    let preview = TransferPreview {
        month,
        total_hours: round2(lines.iter().map(|l| l.paid_hours).sum()),
        total_cost_cents: lines.iter().map(|l| l.cost_cents).sum(),
        transferred_at: existing.iter().map(|e| e.transferred_at).max(),
        has_changes: lines.iter().any(|l| l.changed),
        month_complete: end < today_berlin(),
        lines,
    };
    Ok(TransferData { preview, inputs })
}

pub(crate) async fn transfer_preview(pool: &PgPool, month: NaiveDate) -> Result<TransferPreview, ApiError> {
    Ok(transfer_data(pool, month).await?.preview)
}

/// Freeze a month's hours into `labor_months`.
///
/// **Why the lock**: the snapshot is computed from reads (live hours, the rate a
/// month was first costed at) and then written. Two transfers of the same month
/// running side by side (two admins, two tabs) could each write a different
/// snapshot, and the older one could win. A transaction-scoped advisory lock per
/// month makes them run one after the other; it is released on commit, rollback
/// or when the request is dropped. The key includes the tenant, so companies never wait
/// on each other.
///
/// The response is built from this one read plus the rows just written, so it is
/// exactly what a reload would show without computing the month twice.
pub(crate) async fn transfer(pool: &PgPool, month: NaiveDate, actor: &str) -> Result<TransferPreview, ApiError> {
    let mut lock = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_tenant_id()::text || ':' || $1, 0))")
        .bind(format!("labor_transfer:{month}"))
        .execute(&mut *lock)
        .await?;
    let data = transfer_data(pool, month).await?;
    let written = accounting_repo::replace_labor_month(pool, month, &data.inputs, actor).await?;
    lock.commit().await?;
    Ok(settled_preview(data.preview, &written))
}

/// The preview as it reads once `written` is the month's snapshot: every line is
/// "transferred and unchanged"; lines without a stored row (nothing worked, old
/// row deleted) drop out, as they would on a fresh read.
fn settled_preview(mut p: TransferPreview, written: &[LaborMonthRow]) -> TransferPreview {
    p.lines.retain_mut(|l| match written.iter().find(|w| w.employee_id == l.employee_id) {
        Some(w) => {
            l.transferred_hours = Some(w.paid_hours);
            l.transferred_cost_cents = Some(w.cost_cents);
            l.changed = false;
            true
        }
        None => false,
    });
    p.total_hours = round2(p.lines.iter().map(|l| l.paid_hours).sum());
    p.total_cost_cents = p.lines.iter().map(|l| l.cost_cents).sum();
    p.transferred_at = written.iter().map(|w| w.transferred_at).max();
    p.has_changes = false;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn labor(e: Uuid, month: NaiveDate, hours: f64) -> LaborMonthRow {
        LaborMonthRow {
            id: Uuid::now_v7(),
            employee_id: e,
            month,
            paid_hours: hours,
            worked_hours: hours,
            rate_cents: 1850,
            cost_cents: cents(hours, 1850),
            unconfirmed_days: 0,
            breakdown: serde_json::json!([]),
            transferred_at: chrono::Utc::now(),
            transferred_by: None,
        }
    }

    fn wage(e: Option<Uuid>, month: NaiveDate, c: i64) -> WagesRow {
        WagesRow { period_month: month, employee_id: e, netto_cents: c }
    }

    #[test]
    fn no_data_falls_back_to_default() {
        let r = compute_rates(&[], &[], 1850);
        assert_eq!(r.company_rate(), (1850, RateSource::Standard));
        assert_eq!(r.for_employee(Uuid::now_v7()), (1850, RateSource::Standard));
    }

    #[test]
    fn real_rate_is_wages_over_transferred_hours() {
        let a = Uuid::now_v7();
        let m = d(2026, 8, 1);
        // 100 h logged, €2,000 paid → €20/h: 8 % of paid time never reached a job.
        let r = compute_rates(&[labor(a, m, 100.0)], &[wage(Some(a), m, 200_000)], 1850);
        assert_eq!(r.for_employee(a), (2000, RateSource::Belegt));
        assert_eq!(r.company.as_ref().unwrap().rate_cents, 2000);
    }

    #[test]
    fn unlinked_wages_are_spread_by_hours() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let m = d(2026, 8, 1);
        // Net wages linked, one unlinked SV transfer of €300 for both.
        let r = compute_rates(
            &[labor(a, m, 100.0), labor(b, m, 50.0)],
            &[wage(Some(a), m, 150_000), wage(Some(b), m, 75_000), wage(None, m, 30_000)],
            1850,
        );
        // a: 1500 + 200 = 1700 / 100 h; b: 750 + 100 = 850 / 50 h
        assert_eq!(r.for_employee(a).0, 1700);
        assert_eq!(r.for_employee(b).0, 1700);
        assert_eq!(r.company.unwrap().rate_cents, 1700);
    }

    #[test]
    fn partial_wage_booking_does_not_dilute_the_company_rate() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let m = d(2026, 8, 1);
        // Only a's Lohn is booked so far; b's 300 h must not drag the rate to ~5 €/h.
        let r = compute_rates(&[labor(a, m, 100.0), labor(b, m, 300.0)], &[wage(Some(a), m, 200_000)], 1850);
        assert_eq!(r.company_rate(), (2000, RateSource::Betrieb));
        assert_eq!(r.for_employee(b), (2000, RateSource::Betrieb));
    }

    #[test]
    fn month_labor_mixes_booked_wages_and_estimates() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let m = d(2026, 8, 1);
        let la = labor(a, m, 100.0);
        let lb = labor(b, m, 50.0);
        let w = wage(Some(a), m, 200_000);
        let rates = compute_rates(&[], &[], 1850);
        let (cost, src, hours) = month_labor(&[&la, &lb], &HashMap::new(), &[&w], &rates);
        // a: booked 2,000 €; b: snapshot 50 h × 18.50 = 925 €.
        assert_eq!(cost, 200_000 + 92_500);
        assert_eq!(src, LaborSource::Uebernommen);
        assert_eq!(hours, 150.0);
        let wb = wage(Some(b), m, 100_000);
        let (cost, src, _) = month_labor(&[&la, &lb], &HashMap::new(), &[&w, &wb], &rates);
        assert_eq!((cost, src), (300_000, LaborSource::Gebucht));
    }

    #[test]
    fn employee_without_own_wages_uses_company_rate() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let m = d(2026, 8, 1);
        let r = compute_rates(&[labor(a, m, 100.0)], &[wage(Some(a), m, 200_000)], 1850);
        assert_eq!(r.for_employee(b), (2000, RateSource::Betrieb));
    }

    #[test]
    fn months_without_wages_or_hours_are_skipped_and_window_is_capped() {
        let a = Uuid::now_v7();
        let mut l = Vec::new();
        let mut w = Vec::new();
        for i in 0..8 {
            let m = add_months(d(2026, 1, 1), i);
            l.push(labor(a, m, 100.0));
            // Old months were expensive, recent ones cheap; month 3 has no wages yet.
            if i != 3 {
                w.push(wage(Some(a), m, if i < 2 { 300_000 } else { 190_000 }));
            }
        }
        let r = compute_rates(&l, &w, 1850);
        let real = r.per_employee.get(&a).unwrap();
        assert_eq!(real.months, RATE_WINDOW_MONTHS);
        // Newest 6 qualifying: months 7,6,5,4,2,1 → five at 1900 + one at 3000.
        assert_eq!(real.rate_cents, ((5 * 190_000 + 300_000) as f64 / 600.0).round() as i64);
    }

    fn crew(e: Uuid, src: Uuid, day: NaiveDate, actual: Option<f64>, planned: f64, confirmed: bool) -> CrewDayRow {
        CrewDayRow {
            employee_id: e,
            kind: "inquiry".into(),
            source_id: src,
            inquiry_id: Some(src),
            day,
            actual_hours: actual,
            planned_hours: Some(planned),
            confirmed,
        }
    }

    #[test]
    fn paid_days_apply_overrides_and_plan() {
        let e = Uuid::now_v7();
        let job = Uuid::now_v7();
        let today = d(2026, 9, 15);
        let rows = vec![
            crew(e, job, d(2026, 9, 10), Some(8.0), 6.0, true),
            crew(e, job, d(2026, 9, 11), Some(8.0), 6.0, true),
            crew(e, job, d(2026, 9, 12), None, 6.0, false),
            crew(e, job, d(2026, 9, 20), None, 5.0, false),
        ];
        let adj = vec![BulkAdjustmentRow {
            employee_id: e,
            entry_type: "inquiry".into(),
            inquiry_id: Some(job),
            calendar_item_id: None,
            job_date: d(2026, 9, 11),
            deactivated: true,
            paid_clock_in: None,
            paid_clock_out: None,
            paid_break_minutes: None,
        }];
        let days = paid_days(&rows, &adj, today);
        assert_eq!(days[0].paid_hours, 8.0);
        assert_eq!(days[1].state, DayState::Deactivated);
        assert_eq!(days[1].paid_hours, 0.0);
        assert_eq!(days[2].state, DayState::Unconfirmed);
        assert_eq!(days[2].paid_hours, 0.0);
        assert_eq!(days[2].projected_hours, 6.0);
        assert_eq!(days[3].state, DayState::Planned);
        assert_eq!(days[3].projected_hours, 5.0);
        let s = sum_by_employee(&days);
        assert_eq!(s[&e].paid_hours, 8.0);
        assert_eq!(s[&e].unconfirmed_days, 1);
        assert_eq!(s[&e].planned_days, 1);
    }

    fn mrow(month: NaiveDate, rev: i64, labor: i64, fixed: i64, var: i64) -> MonthRow {
        MonthRow {
            month,
            revenue_cents: rev,
            labor_cents: labor,
            labor_source: LaborSource::Gebucht,
            labor_hours: 0.0,
            fixed_cents: fixed,
            variable_cents: var,
            draft_cents: 0,
            result_cents: rev - labor - fixed - var,
            is_future: false,
            is_current: false,
        }
    }

    fn rate_month(month: NaiveDate, hours: f64, wages: i64, costs: &[(&str, &str, bool, i64)]) -> RateMonth {
        RateMonth {
            month,
            sold_hours: hours,
            wage_cents: wages,
            wages_booked: true,
            costs: costs.iter().map(|(n, k, i, c)| (n.to_string(), k.to_string(), *i, *c)).collect(),
        }
    }

    /// The worked example from the design discussion: w = 19.20, v = 1.50,
    /// F = 5,600 €/month, 450 h → 33.14 €/h; at 30 €/h Alex needs ~602 h/month.
    #[test]
    fn hourly_rate_matches_the_worked_example() {
        let costs = [
            ("Miete", "fixed", true, 350_000),
            ("Finanzierung / Kredit", "fixed", true, 210_000),
            ("Reparatur & Wartung", "variable", true, 67_500),
            ("Kraftstoff", "variable", false, 90_000), // out: covered by the km charge
            ("Löhne", "wages", true, 999_999),          // ignored: wages come from labor
        ];
        let months: Vec<RateMonth> = vec![
            rate_month(d(2026, 1, 1), 0.0, 0, &costs[..2]), // before tracking — skipped
            rate_month(d(2026, 2, 1), 450.0, 864_000, &costs),
            rate_month(d(2026, 3, 1), 450.0, 864_000, &costs),
        ];
        let mut settings = accounting_repo::HourlyCalcSettings::default();
        settings.target_profit_cents = 300_000;
        settings.capacity_crew = Some(5);

        let r = compute_hourly_rate(&months, &settings, 3000, 7).unwrap();
        assert_eq!(r.months.len(), 2);
        assert_eq!(r.wage_per_hour_cents, 1920);
        assert_eq!(r.variable_per_hour_cents, 150);
        assert_eq!(r.fixed_per_month_cents, 560_000);
        assert_eq!(r.excluded_per_month_cents, 90_000);
        assert_eq!(r.break_even_cents, 3314);
        // +3,000 € target profit: 20.70 + 8,600 / 450
        assert_eq!(r.target_rate_cents, 3981);
        assert_eq!(r.hours_needed_break_even, Some(602.2));
        // At 30 €/h and 450 h: 9.30 × 450 − 5,600 = −1,415 € a month.
        assert_eq!(r.result_at_current_cents, -141_500);
        // Comparison line: 5 × 8 h × 21 days = 840 h → 20.70 + 6.67.
        assert_eq!(r.capacity_hours, 840.0);
        assert_eq!(r.capacity_break_even_cents, 2737);
        assert!(r.inaccurate, "two months of data must be flagged");
        assert_eq!(r.components[0].label, "Löhne");
        assert!(r.volumes.iter().any(|v| v.kind == "capacity" && v.hours == 840.0));
        assert!(r.volumes.windows(2).all(|w| w[0].hours < w[1].hours));
    }

    #[test]
    fn hourly_rate_needs_sold_hours_and_flags_a_losing_rate() {
        let settings = accounting_repo::HourlyCalcSettings::default();
        assert!(compute_hourly_rate(&[rate_month(d(2026, 1, 1), 0.0, 0, &[])], &settings, 3000, 3).is_none());
        // Wages alone cost more than the rate: no volume ever breaks even.
        let r = compute_hourly_rate(&[rate_month(d(2026, 1, 1), 100.0, 320_000, &[])], &settings, 3000, 3).unwrap();
        assert_eq!(r.hours_needed_break_even, None);
    }

    fn pos_invoice(invoice_type: &str, pct: Option<i32>, manual: bool, items: serde_json::Value, extras: serde_json::Value) -> accounting_repo::PositionInvoiceRow {
        accounting_repo::PositionInvoiceRow {
            invoice_type: invoice_type.into(),
            partial_percent: pct,
            deposit_percent: None,
            is_manual: manual,
            line_items_json: if manual { Some(items.clone()) } else { None },
            extra_services: extras,
            offer_line_items: if manual { None } else { Some(items) },
            service_date: Some(d(2026, 9, 2)),
        }
    }

    #[test]
    fn invoice_positions_split_kva_lines_by_the_invoices_share() {
        // Real KVA shape from staging: labor skipped, flat_total wins over qty × price.
        let kva = serde_json::json!([
            {"description": "3 Umzugshelfer", "is_labor": true, "quantity": 7.0, "unit_price": 35.0, "flat_total": null},
            {"description": "Verkauf U-Karton", "is_labor": false, "quantity": 80.0, "unit_price": 2.5, "flat_total": null},
            {"description": "Fahrkostenpauschale", "is_labor": false, "quantity": 0.0, "unit_price": 0.0, "flat_total": 150.0}
        ]);
        let full = invoice_positions(&pos_invoice("full", None, false, kva.clone(), serde_json::json!([{"description": "Extra", "price_cents": 5000}])));
        assert_eq!(full, vec![("Verkauf U-Karton".into(), 20_000), ("Fahrkostenpauschale".into(), 15_000), ("Extra".into(), 5_000)]);

        // Anzahlung 30 % + Schlussrechnung 70 % add up to the KVA; extras only on the final.
        let first = invoice_positions(&pos_invoice("partial_first", Some(30), false, kva.clone(), serde_json::json!([{"description": "Extra", "price_cents": 5000}])));
        let fin = invoice_positions(&pos_invoice("partial_final", Some(30), false, kva, serde_json::json!([])));
        assert_eq!(first[0].1 + fin[0].1, 20_000);
        assert_eq!(first.len(), 2);

        let manual = invoice_positions(&pos_invoice(
            "full",
            None,
            true,
            serde_json::json!([{"description": " Fahrkostenpauschale ", "quantity": 156.0, "unit_price_cents": 50}]),
            serde_json::json!([]),
        ));
        assert_eq!(manual, vec![("Fahrkostenpauschale".into(), 7_800)]);
    }

    #[test]
    fn recharge_verdicts() {
        assert_eq!(verdict(124_000, 41_000), RechargeVerdict::Gewinn);
        assert_eq!(verdict(60_000, 60_200), RechargeVerdict::Durchlauf);
        assert_eq!(verdict(30_000, 45_000), RechargeVerdict::Verlust);
        assert_eq!(verdict(0, 19_000), RechargeVerdict::Unklar);
        assert_eq!(norm("  Verkauf   U-Karton "), "verkauf u-karton");
    }

    #[test]
    fn break_even_uses_last_three_complete_months() {
        let rows = vec![
            mrow(d(2026, 5, 1), 1_000_000, 900_000, 0, 0), // outside the window
            mrow(d(2026, 6, 1), 1_000_000, 400_000, 300_000, 100_000),
            mrow(d(2026, 7, 1), 1_000_000, 400_000, 300_000, 100_000),
            mrow(d(2026, 8, 1), 1_000_000, 400_000, 300_000, 100_000),
            mrow(d(2026, 9, 1), 0, 0, 300_000, 0), // current month
        ];
        let be = compute_break_even(&rows, d(2026, 9, 1)).unwrap();
        // 50 % of revenue left after labor+variable → 3,000 € fixed needs 6,000 €.
        assert_eq!(be.contribution_ratio, 0.5);
        assert_eq!(be.revenue_cents, 600_000);
        assert_eq!(be.months.len(), 3);
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::repositories::accounting_repo::{ExpenseInput, RecurringInput};
    use crate::test_helpers;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    async fn category(pool: &PgPool, name: &str) -> Uuid {
        sqlx::query_scalar("SELECT id FROM expense_categories WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn expense(category_id: Uuid, month: NaiveDate, brutto: i64, vat: i16) -> ExpenseInput {
        let netto = (brutto as f64 * 100.0 / (100.0 + vat as f64)).round() as i64;
        ExpenseInput {
            category_id,
            status: "booked".into(),
            receipt_date: month,
            paid_on: None,
            period_month: month,
            supplier: Some("Tankstelle".into()),
            receipt_number: None,
            description: None,
            netto_cents: netto,
            vat_rate: vat,
            vat_cents: brutto - netto,
            brutto_cents: brutto,
            vehicle_id: None,
            inquiry_id: None,
            employee_id: None,
        }
    }

    async fn audit_actions(pool: &PgPool, id: Uuid) -> Vec<String> {
        sqlx::query_scalar("SELECT action FROM accounting_audit_log WHERE entity_id = $1 ORDER BY created_at")
            .bind(id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn storno_nets_out_and_delete_takes_the_storno_along(pool: PgPool) {
        let fuel = category(&pool, "Kraftstoff").await;
        let m = d(2025, 3, 1);
        let e = accounting_repo::insert_expense(&pool, &expense(fuel, m, 11_900, 19), "alex").await.unwrap();
        assert_eq!(e.netto_cents, 10_000);

        let s = accounting_repo::storno_expense(&pool, e.id, "alex").await.unwrap();
        assert_eq!(s.netto_cents, -10_000);
        assert_eq!(s.storno_of, Some(e.id));
        let costs = accounting_repo::cost_by_kind(&pool, m, m).await.unwrap();
        assert_eq!(costs.iter().map(|c| c.netto_cents).sum::<i64>(), 0);

        // Frozen once reversed; a second Storno is refused.
        assert!(accounting_repo::update_expense(&pool, e.id, &expense(fuel, m, 5_000, 19), "alex").await.is_err());
        assert!(accounting_repo::storno_expense(&pool, e.id, "alex").await.is_err());

        // Hard delete is still allowed and must not leave the negative twin behind.
        accounting_repo::delete_expense(&pool, e.id, "alex").await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM expenses").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);

        // The log outlives the rows.
        assert_eq!(audit_actions(&pool, e.id).await, vec!["create", "storno", "delete"]);
        assert_eq!(audit_actions(&pool, s.id).await, vec!["delete"]);
    }

    /// Deleting only the negative twin would bring the reversed cost back unnoticed.
    #[sqlx::test(migrations = "../../migrations")]
    async fn storno_row_cannot_be_deleted_on_its_own(pool: PgPool) {
        let fuel = category(&pool, "Kraftstoff").await;
        let m = d(2025, 3, 1);
        let e = accounting_repo::insert_expense(&pool, &expense(fuel, m, 11_900, 19), "alex").await.unwrap();
        let s = accounting_repo::storno_expense(&pool, e.id, "alex").await.unwrap();

        assert!(matches!(accounting_repo::delete_expense(&pool, s.id, "alex").await, Err(ApiError::Conflict(_))));
        let costs = accounting_repo::cost_by_kind(&pool, m, m).await.unwrap();
        assert_eq!(costs.iter().map(|c| c.netto_cents).sum::<i64>(), 0);
    }

    /// GoBD-readiness: the bulk paths (generated drafts, the drafts a template edit or
    /// delete touches, a new category) each leave one audit entry per row.
    #[sqlx::test(migrations = "../../migrations")]
    async fn bulk_writes_are_audited_row_by_row(pool: PgPool) {
        let cat = accounting_repo::insert_category(&pool, "Lagerhalle", "fixed", 19, "alex").await.unwrap();
        assert_eq!(audit_actions(&pool, cat.id).await, vec!["create"]);

        let mut input = RecurringInput {
            category_id: cat.id,
            label: "Halle".into(),
            supplier: None,
            netto_cents: 100_000,
            vat_rate: 19,
            vat_cents: 19_000,
            brutto_cents: 119_000,
            interval_months: 1,
            day_of_month: 1,
            start_month: d(2025, 1, 1),
            end_month: None,
            vehicle_id: None,
            active: true,
            notes: None,
        };
        let r = accounting_repo::insert_recurring(&pool, &input, "alex").await.unwrap();
        assert_eq!(accounting_repo::generate_recurring_drafts(&pool, d(2025, 3, 1)).await.unwrap(), 3);
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM expenses WHERE recurring_id = $1 ORDER BY period_month")
            .bind(r.id)
            .fetch_all(&pool)
            .await
            .unwrap();
        let actor: String = sqlx::query_scalar("SELECT actor FROM accounting_audit_log WHERE entity_id = $1")
            .bind(ids[0])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(actor, "dauerauftrag");

        // Price change + start moved to February: January's draft goes, the others follow.
        input.netto_cents = 120_000;
        input.start_month = d(2025, 2, 1);
        accounting_repo::update_recurring(&pool, r.id, &input, "alex").await.unwrap();
        assert_eq!(audit_actions(&pool, ids[0]).await, vec!["create", "delete"]);
        assert_eq!(audit_actions(&pool, ids[1]).await, vec!["create", "update"]);

        // Saving again without a change logs nothing more for the drafts.
        accounting_repo::update_recurring(&pool, r.id, &input, "alex").await.unwrap();
        assert_eq!(audit_actions(&pool, ids[1]).await, vec!["create", "update"]);

        accounting_repo::delete_recurring(&pool, r.id, "alex").await.unwrap();
        assert_eq!(audit_actions(&pool, ids[2]).await, vec!["create", "update", "delete"]);
    }

    /// Moving a Dauerauftrag's day re-dates its open drafts; booked months keep theirs.
    #[sqlx::test(migrations = "../../migrations")]
    async fn changing_day_of_month_redates_open_drafts(pool: PgPool) {
        let rent = category(&pool, "Miete").await;
        let mut input = RecurringInput {
            category_id: rent,
            label: "Miete".into(),
            supplier: None,
            netto_cents: 100_000,
            vat_rate: 19,
            vat_cents: 19_000,
            brutto_cents: 119_000,
            interval_months: 1,
            day_of_month: 1,
            start_month: d(2025, 1, 1),
            end_month: None,
            vehicle_id: None,
            active: true,
            notes: None,
        };
        let r = accounting_repo::insert_recurring(&pool, &input, "alex").await.unwrap();
        accounting_repo::generate_recurring_drafts(&pool, d(2025, 2, 1)).await.unwrap();
        let all = |pool: PgPool| async move {
            accounting_repo::list_expenses(&pool, &accounting_repo::ExpenseFilter::default()).await.unwrap()
        };
        let jan = all(pool.clone()).await.into_iter().find(|e| e.period_month == d(2025, 1, 1)).unwrap();
        accounting_repo::confirm_expense(&pool, jan.id, "alex").await.unwrap();

        input.day_of_month = 28;
        accounting_repo::update_recurring(&pool, r.id, &input, "alex").await.unwrap();

        let rows = all(pool.clone()).await;
        let date_of = |m: NaiveDate| rows.iter().find(|e| e.period_month == m).unwrap().receipt_date;
        assert_eq!(date_of(d(2025, 1, 1)), d(2025, 1, 1), "booked month keeps its date");
        assert_eq!(date_of(d(2025, 2, 1)), d(2025, 2, 28), "open draft follows the template");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn recurring_drafts_are_idempotent_and_respect_interval(pool: PgPool) {
        let rent = category(&pool, "Miete").await;
        let insurance = category(&pool, "Versicherung").await;
        let base = RecurringInput {
            category_id: rent,
            label: "Miete Lagerhalle".into(),
            supplier: None,
            netto_cents: 100_000,
            vat_rate: 19,
            vat_cents: 19_000,
            brutto_cents: 119_000,
            interval_months: 1,
            day_of_month: 3,
            start_month: d(2025, 1, 1),
            end_month: None,
            vehicle_id: None,
            active: true,
            notes: None,
        };
        accounting_repo::insert_recurring(&pool, &base, "alex").await.unwrap();
        accounting_repo::insert_recurring(
            &pool,
            &RecurringInput {
                category_id: insurance,
                label: "Kfz-Versicherung".into(),
                interval_months: 3,
                vat_rate: 0,
                vat_cents: 0,
                netto_cents: 30_000,
                brutto_cents: 30_000,
                ..base.clone()
            },
            "alex",
        )
        .await
        .unwrap();

        // Jan–Jun: 6 rent + 2 quarterly (Jan, Apr).
        assert_eq!(accounting_repo::generate_recurring_drafts(&pool, d(2025, 6, 1)).await.unwrap(), 8);
        assert_eq!(accounting_repo::generate_recurring_drafts(&pool, d(2025, 6, 1)).await.unwrap(), 0);

        let drafts = accounting_repo::list_expenses(
            &pool,
            &accounting_repo::ExpenseFilter { status: Some("draft".into()), ..Default::default() },
        )
        .await
        .unwrap();
        assert_eq!(drafts.len(), 8);
        assert!(drafts.iter().all(|e| e.receipt_date.day() == 3));

        // Confirming makes it a booking; a deleted draft is not resurrected.
        let june = drafts.iter().find(|e| e.period_month == d(2025, 6, 1) && e.category_id == rent).unwrap();
        accounting_repo::confirm_expense(&pool, june.id, "alex").await.unwrap();
        let may = drafts.iter().find(|e| e.period_month == d(2025, 5, 1) && e.category_id == rent).unwrap();
        accounting_repo::delete_expense(&pool, may.id, "alex").await.unwrap();
        assert_eq!(accounting_repo::generate_recurring_drafts(&pool, d(2025, 6, 1)).await.unwrap(), 0);
    }

    /// Staging 2026-10-03: "Miete Büro" was created with start October, then moved to
    /// January — and January–September never appeared, because the generator refused
    /// to fill months before the template's newest entry.
    #[sqlx::test(migrations = "../../migrations")]
    async fn moving_the_start_month_backfills_and_forward_prunes(pool: PgPool) {
        let rent = category(&pool, "Miete").await;
        let mut input = RecurringInput {
            category_id: rent,
            label: "Miete Büro".into(),
            supplier: None,
            netto_cents: 50_000,
            vat_rate: 19,
            vat_cents: 9_500,
            brutto_cents: 59_500,
            interval_months: 1,
            day_of_month: 1,
            start_month: d(2026, 10, 1),
            end_month: None,
            vehicle_id: None,
            active: true,
            notes: None,
        };
        let r = accounting_repo::insert_recurring(&pool, &input, "alex").await.unwrap();
        assert_eq!(accounting_repo::generate_recurring_drafts(&pool, d(2026, 10, 1)).await.unwrap(), 1);

        input.start_month = d(2026, 1, 1);
        accounting_repo::update_recurring(&pool, r.id, &input, "alex").await.unwrap();
        assert_eq!(accounting_repo::generate_recurring_drafts(&pool, d(2026, 10, 1)).await.unwrap(), 9);

        // Moving it later again drops the drafts that no longer apply.
        input.start_month = d(2026, 7, 1);
        accounting_repo::update_recurring(&pool, r.id, &input, "alex").await.unwrap();
        let months: Vec<NaiveDate> = sqlx::query_scalar(
            "SELECT period_month FROM expenses WHERE recurring_id = $1 ORDER BY period_month",
        )
        .bind(r.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(months, vec![d(2026, 7, 1), d(2026, 8, 1), d(2026, 9, 1), d(2026, 10, 1)]);
    }

    async fn seed_job(pool: &PgPool, day: NaiveDate) -> Uuid {
        let id = test_helpers::insert_test_quote_with_status(pool, "completed").await;
        sqlx::query("UPDATE inquiries SET scheduled_date = $2 WHERE id = $1")
            .bind(id)
            .bind(day)
            .execute(pool)
            .await
            .unwrap();
        test_helpers::insert_test_offer(pool, id, "accepted").await; // 500,00 € netto, 2 × 4 h
        id
    }

    async fn log_hours(pool: &PgPool, job: Uuid, emp: Uuid, day: NaiveDate, hours: f64) {
        test_helpers::insert_test_inquiry_employee(pool, job, emp, day, hours).await;
        sqlx::query(
            "UPDATE inquiry_employees SET clock_in = '08:00', clock_out = '17:00', actual_hours = $4
             WHERE inquiry_id = $1 AND employee_id = $2 AND job_date = $3",
        )
        .bind(job)
        .bind(emp)
        .bind(day)
        .bind(hours)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A second transfer of the same month waits until the first one is done.
    #[sqlx::test(migrations = "../../migrations")]
    async fn transfers_of_one_month_run_one_after_the_other(pool: PgPool) {
        let month = add_months(month_start(today_berlin()), -2);
        let mut holder = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_tenant_id()::text || ':' || $1, 0))")
            .bind(format!("labor_transfer:{month}"))
            .execute(&mut *holder)
            .await
            .unwrap();

        let p = pool.clone();
        let waiting = aust_core::tenant::spawn(async move { transfer(&p, month, "alex").await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!waiting.is_finished(), "transfer ran while the month was locked");

        holder.commit().await.unwrap();
        assert!(waiting.await.unwrap().is_ok());
        // Another month is never blocked.
        transfer(&pool, add_months(month, -1), "alex").await.unwrap();
    }

    /// The core promise: cost starts at €18.50 × hours and becomes the real rate once
    /// Alex transfers the month's hours and books the wages.
    #[sqlx::test(migrations = "../../migrations")]
    async fn labor_cost_is_extrapolated_from_transferred_hours_and_booked_wages(pool: PgPool) {
        let anna = test_helpers::insert_test_employee(&pool, "Anna", "Arbeit").await;
        let ben = test_helpers::insert_test_employee(&pool, "Ben", "Bauer").await;
        // Two months back: complete, and inside the rate window whenever this runs.
        let march = add_months(month_start(today_berlin()), -2);
        let day = march + chrono::Days::new(9);
        let job = seed_job(&pool, day).await;
        log_hours(&pool, job, anna, day, 8.0).await;
        log_hours(&pool, job, ben, day, 8.0).await;

        // Before anything: default rate on the job.
        let before = jobs(&pool, march).await.unwrap();
        let j = &before.jobs[0];
        assert_eq!(j.revenue_cents, 50_000);
        assert!(matches!(j.revenue_source, RevenueSource::Angebot));
        assert_eq!(j.labor_cents, 2 * 8 * 1850);
        assert!(j.crew.iter().all(|c| c.rate_source == RateSource::Standard));

        // Transfer March.
        let preview = transfer_preview(&pool, march).await.unwrap();
        assert_eq!(preview.total_hours, 16.0);
        assert!(preview.has_changes);
        let done = transfer(&pool, march, "alex").await.unwrap();
        assert!(!done.has_changes);
        assert!(done.transferred_at.is_some());
        // Built without a second read, the answer must still equal a reload.
        assert_eq!(
            serde_json::to_value(&done).unwrap(),
            serde_json::to_value(transfer_preview(&pool, march).await.unwrap()).unwrap()
        );

        // Later edits in the hours tab don't move the frozen month…
        sqlx::query("UPDATE inquiry_employees SET actual_hours = 10 WHERE employee_id = $1")
            .bind(anna)
            .execute(&pool)
            .await
            .unwrap();
        let again = transfer_preview(&pool, march).await.unwrap();
        let anna_line = again.lines.iter().find(|l| l.employee_id == anna).unwrap();
        assert_eq!(anna_line.transferred_hours, Some(8.0));
        assert_eq!(anna_line.paid_hours, 10.0);
        assert!(anna_line.changed);
        // …and the job margin keeps using the frozen 8 h.
        assert_eq!(jobs(&pool, march).await.unwrap().jobs[0].labor_hours, 16.0);

        // Book wages: Anna €176 linked, plus one unlinked SV payment of €64 for both.
        let wages = category(&pool, "Löhne").await;
        let mut w = expense(wages, march, 17_600, 0);
        w.employee_id = Some(anna);
        accounting_repo::insert_expense(&pool, &w, "alex").await.unwrap();
        let mut ben_w = expense(wages, march, 14_800, 0);
        ben_w.employee_id = Some(ben);
        accounting_repo::insert_expense(&pool, &ben_w, "alex").await.unwrap();
        accounting_repo::insert_expense(&pool, &expense(wages, march, 6_400, 0), "alex").await.unwrap();

        let rates = load_rates(&pool).await.unwrap();
        // Anna: (176 + 32) / 8 h = 26 €/h — well above 18.50, paid time is missing.
        assert_eq!(rates.for_employee(anna), (2600, RateSource::Belegt));
        // Ben: (148 + 32) / 8 h = 22.50 €/h.
        assert_eq!(rates.for_employee(ben), (2250, RateSource::Belegt));
        assert_eq!(rates.company_rate(), (2425, RateSource::Betrieb));

        let after = jobs(&pool, march).await.unwrap();
        assert_eq!(after.jobs[0].labor_cents, 8 * 2600 + 8 * 2250);

        // A new KVA is previewed at the company's real rate.
        let fresh = test_helpers::insert_test_quote_with_status(&pool, "offer_ready").await;
        test_helpers::insert_test_offer(&pool, fresh, "draft").await;
        let preview = inquiry_margin(&pool, fresh, 3000).await.unwrap();
        let est = preview.estimate.unwrap();
        assert_eq!(est.labor_cents, 8 * 2425);
        assert_eq!(est.rate_source, RateSource::Betrieb);

        // The month's labor line is now the actual booked money.
        let ov = overview(&pool, march.year()).await.unwrap();
        let mar = &ov.months[march.month0() as usize];
        assert_eq!(mar.labor_source, LaborSource::Gebucht);
        assert_eq!(mar.labor_cents, 38_800);

        // Stundensatz: wages 388 € over 16 sold hours → w = 24.25 €/h; no fixed costs yet.
        let hr = hourly_rate(&pool, 3000).await.unwrap().unwrap();
        assert_eq!(hr.wage_per_hour_cents, 2425);
        assert!(hr.warnings.iter().any(|w| w.contains("Fixkosten")));

        let emp = employees(&pool).await.unwrap();
        let anna_cost = emp.employees.iter().find(|e| e.employee_id == anna).unwrap();
        assert!(anna_cost.warnings.iter().any(|w| w.contains("über dem Standard")));
    }
}
