//! Conversion tab — which channel brings customers, and where they drop off.
//!
//! **Caller**: `GET /api/v1/admin/conversion` ([`crate::routes::conversion`]).
//!
//! A *lead* is one person getting in touch: a website form mail (Kontaktformular or
//! Kostenloses Angebot), a Rückruf request, or — when nothing online preceded it — an
//! inquiry Alex typed in himself, which is a phone call. Forms usually end up as a
//! hand-made inquiry too (Alex calls back and creates it), so each inquiry is first
//! *attributed* to the earliest form mail / Rückruf from the same person; only the
//! inquiries nobody claimed count as Telefon.
//!
//! "Same person" = same customer (after merges), same e-mail, or same phone number
//! (last 9 digits). The inquiry has to be created between 2 days before and 60 days
//! after the submission; a repeat customer months later is a new lead.
//!
//! Each lead then walks the funnel Eingang → Anfrage → KVA erstellt → KVA versendet →
//! Auftrag → Durchgeführt → Bezahlt and ends as won, lost (with a reason) or open.
//! Everything here is pure; [`build_report`] takes the rows and `now` so it is tested
//! without a database.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::ApiError;
use crate::repositories::conversion_repo::{
    self, FlashContactRow, FormSubmissionRow, InquiryFactRow,
};

/// How far before a submission an inquiry may already exist (Alex sometimes creates
/// it while still on the phone, before the confirmation mail arrives).
const MATCH_BEFORE: Duration = Duration::days(2);
/// How long after a submission the inquiry may be created and still count for it.
const MATCH_AFTER: Duration = Duration::days(60);
/// A second form from the same person within this window is a duplicate, not a lead.
const DUPLICATE_WINDOW: Duration = Duration::days(14);
/// A submission with no inquiry after this long is a drop-off ("keine Anfrage").
const NO_INQUIRY_AFTER: Duration = Duration::days(14);
/// An inquiry without a sent KVA after this long is a drop-off ("kein KVA").
const NO_KVA_AFTER: Duration = Duration::days(30);
/// A sent KVA without answer after this long is a drop-off ("keine Rückmeldung").
const NO_ANSWER_AFTER: Duration = Duration::days(30);

/// Funnel stages, in order. Serialized as their index (0 = Eingang).
pub(crate) const STAGE_COUNT: usize = 7;
const STAGE_INQUIRY: u8 = 1;
const STAGE_KVA: u8 = 2;
const STAGE_SENT: u8 = 3;
const STAGE_ORDER: u8 = 4;
const STAGE_DONE: u8 = 5;
const STAGE_PAID: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Channel {
    Telefon,
    Kontaktformular,
    Angebotsformular,
    Rueckruf,
    Email,
    App,
}

/// Fixed display order — the frontend colours channels by this order.
pub(crate) const CHANNELS: [Channel; 6] = [
    Channel::Telefon,
    Channel::Kontaktformular,
    Channel::Angebotsformular,
    Channel::Rueckruf,
    Channel::Email,
    Channel::App,
];

impl Channel {
    /// Channel of an inquiry no submission claimed, from `inquiries.source`.
    fn from_source(source: Option<&str>) -> Self {
        match source.unwrap_or("") {
            "quote_form" => Channel::Angebotsformular,
            "contact_form" => Channel::Kontaktformular,
            "direct_email" | "media_email" => Channel::Email,
            "photo_webapp" | "mobile_app" | "mobile" | "app" => Channel::App,
            // admin_dashboard and anything unknown: typed in by Alex → a phone call.
            _ => Channel::Telefon,
        }
    }

    /// Channel of a form mail, from its `send-mail.php` subject.
    fn from_subject(subject: &str) -> Self {
        if subject.starts_with("Neue Kontaktanfrage") {
            Channel::Kontaktformular
        } else {
            Channel::Angebotsformular
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    Won,
    Lost,
    Open,
    /// Second form from someone already counted — listed, never counted.
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LostReason {
    /// Form or Rückruf never turned into an inquiry.
    KeineAnfrage,
    /// Inquiry declined before any KVA went out.
    AbgesagtVorKva,
    /// Inquiry left without a sent KVA.
    KeinKva,
    KvaAbgelehnt,
    KvaAbgelaufen,
    /// KVA sent, no answer for a month.
    KeineRueckmeldung,
    Storniert,
    StorniertNachZusage,
}

/// One person getting in touch, and how far they got.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Lead {
    /// Submission id, or the inquiry id for a phone lead.
    pub id: Uuid,
    pub channel: Channel,
    pub at: DateTime<Utc>,
    pub name: Option<String>,
    pub inquiry_id: Option<Uuid>,
    /// Highest funnel stage reached (0 = Eingang … 6 = Bezahlt).
    pub stage: u8,
    pub outcome: Outcome,
    pub reason: Option<LostReason>,
    /// Netto price of the KVA (shown for every lead that has one).
    pub value_cents: Option<i64>,
    /// Days from first contact to the KVA going out.
    pub days_to_kva: Option<f64>,
    /// Days from the KVA going out to the Zusage.
    pub days_to_order: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ChannelStats {
    /// `None` for the all-channel total.
    pub channel: Option<Channel>,
    /// Counted leads (duplicates excluded).
    pub leads: u32,
    pub duplicates: u32,
    /// `reached[s]` = leads that got at least to stage `s`.
    pub reached: [u32; STAGE_COUNT],
    pub won: u32,
    pub lost: u32,
    pub open: u32,
    pub lost_by_reason: BTreeMap<LostReason, u32>,
    /// Sum of KVA netto over won leads.
    pub won_value_cents: i64,
    pub avg_won_value_cents: Option<i64>,
    pub median_days_to_kva: Option<f64>,
    pub median_days_to_order: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MonthChannel {
    pub channel: Channel,
    pub leads: u32,
    pub won: u32,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MonthRow {
    /// `YYYY-MM`.
    pub month: String,
    pub channels: Vec<MonthChannel>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ConversionReport {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub total: ChannelStats,
    pub channels: Vec<ChannelStats>,
    pub months: Vec<MonthRow>,
    /// Newest first, duplicates included.
    pub leads: Vec<Lead>,
}

/// Loads the rows and builds the report for leads dated `from..=to` (Berlin dates).
pub(crate) async fn report(
    db: &PgPool,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<ConversionReport, ApiError> {
    let (inquiries, forms, flash) = tokio::try_join!(
        conversion_repo::fetch_inquiries(db),
        conversion_repo::fetch_form_submissions(db),
        conversion_repo::fetch_flash_contacts(db),
    )?;
    Ok(build_report(
        &inquiries,
        &forms,
        &flash,
        Utc::now(),
        from,
        to,
    ))
}

// ── Attribution ─────────────────────────────────────────────────────────────

/// Comparable phone key: digits only, +49/0049 folded to 0, last 9 digits.
/// `None` for anything too short to identify a person.
pub(crate) fn phone_key(raw: Option<&str>) -> Option<String> {
    let mut digits: String = raw?.chars().filter(|c| c.is_ascii_digit()).collect();
    if let Some(rest) = digits.strip_prefix("0049") {
        digits = format!("0{rest}");
    } else if digits.starts_with("49") && digits.len() > 11 {
        digits = format!("0{}", &digits[2..]);
    }
    if digits.len() < 7 {
        return None;
    }
    Some(digits[digits.len().saturating_sub(9)..].to_string())
}

fn email_key(raw: Option<&str>) -> Option<String> {
    raw.map(|e| e.trim().to_lowercase())
        .filter(|e| e.contains('@'))
}

/// Who a submission or inquiry belongs to, for "same person" checks.
#[derive(Debug, Clone, Default)]
struct Person {
    customer: Option<Uuid>,
    email: Option<String>,
    phone: Option<String>,
}

impl Person {
    fn same_as(&self, other: &Person) -> bool {
        let eq =
            |a: &Option<String>, b: &Option<String>| matches!((a, b), (Some(x), Some(y)) if x == y);
        matches!((self.customer, other.customer), (Some(a), Some(b)) if a == b)
            || eq(&self.email, &other.email)
            || eq(&self.phone, &other.phone)
    }
}

struct Submission {
    id: Uuid,
    channel: Channel,
    at: DateTime<Utc>,
    name: Option<String>,
    person: Person,
    /// Inquiry the mail pipeline created for it directly.
    direct_inquiry: Option<Uuid>,
    dismissed: bool,
}

fn submissions(forms: &[FormSubmissionRow], flash: &[FlashContactRow]) -> Vec<Submission> {
    let mut out: Vec<Submission> = forms
        .iter()
        .map(|f| Submission {
            id: f.id,
            channel: Channel::from_subject(&f.subject),
            at: f.created_at,
            name: f.name.clone(),
            person: Person {
                customer: f.customer_key,
                email: email_key(f.email.as_deref()),
                phone: phone_key(f.phone.as_deref()),
            },
            direct_inquiry: f.inquiry_id,
            dismissed: false,
        })
        .chain(flash.iter().map(|f| Submission {
            id: f.id,
            channel: Channel::Rueckruf,
            at: f.created_at,
            name: Some(f.name.clone()),
            person: Person {
                customer: None,
                email: None,
                phone: phone_key(Some(&f.phone)),
            },
            direct_inquiry: None,
            dismissed: f.dismissed_at.is_some(),
        }))
        .collect();
    out.sort_by_key(|s| (s.at, s.id));
    out
}

fn inquiry_person(i: &InquiryFactRow) -> Person {
    Person {
        customer: i.customer_key,
        email: email_key(i.customer_email.as_deref()),
        phone: phone_key(i.customer_phone.as_deref()),
    }
}

/// When the KVA went out, from whichever timestamp was recorded.
fn sent_at(i: &InquiryFactRow) -> Option<DateTime<Utc>> {
    i.kva_sent_at.or(i.offer_sent_at)
}

/// Highest funnel stage the inquiry reached.
fn inquiry_stage(i: &InquiryFactRow) -> u8 {
    match i.status.as_str() {
        "paid" => return STAGE_PAID,
        "completed" | "invoiced" => return STAGE_DONE,
        "accepted" | "scheduled" => return STAGE_ORDER,
        _ => {}
    }
    if i.accepted_at.is_some() {
        return STAGE_ORDER;
    }
    let has_kva = i.first_offer_at.is_some();
    let sent = sent_at(i).is_some()
        || i.status == "offer_sent"
        || (matches!(i.status.as_str(), "rejected" | "expired") && has_kva);
    if sent {
        STAGE_SENT
    } else if has_kva {
        STAGE_KVA
    } else {
        STAGE_INQUIRY
    }
}

fn days_between(a: DateTime<Utc>, b: DateTime<Utc>) -> f64 {
    ((b - a).num_minutes() as f64 / 1440.0).max(0.0)
}

/// Lead for an inquiry, first contact at `at` through `channel`.
fn lead_from_inquiry(
    id: Uuid,
    channel: Channel,
    at: DateTime<Utc>,
    name: Option<String>,
    i: &InquiryFactRow,
    now: DateTime<Utc>,
) -> Lead {
    let stage = inquiry_stage(i);
    let (outcome, reason) = match i.status.as_str() {
        // Confirmed once, called off later: the status log knows about the Zusage
        // even when the status now says rejected/expired.
        "cancelled" | "rejected" | "expired" if i.accepted_at.is_some() => {
            (Outcome::Lost, Some(LostReason::StorniertNachZusage))
        }
        "cancelled" => (Outcome::Lost, Some(LostReason::Storniert)),
        _ if stage >= STAGE_ORDER => (Outcome::Won, None),
        "rejected" => (
            Outcome::Lost,
            Some(if stage >= STAGE_SENT {
                LostReason::KvaAbgelehnt
            } else {
                LostReason::AbgesagtVorKva
            }),
        ),
        "expired" => (
            Outcome::Lost,
            Some(if stage >= STAGE_SENT {
                LostReason::KvaAbgelaufen
            } else {
                LostReason::AbgesagtVorKva
            }),
        ),
        _ if stage == STAGE_SENT => {
            let since = sent_at(i).unwrap_or(i.created_at);
            if now - since > NO_ANSWER_AFTER {
                (Outcome::Lost, Some(LostReason::KeineRueckmeldung))
            } else {
                (Outcome::Open, None)
            }
        }
        _ => {
            if now - i.created_at > NO_KVA_AFTER {
                (Outcome::Lost, Some(LostReason::KeinKva))
            } else {
                (Outcome::Open, None)
            }
        }
    };
    let kva_at = sent_at(i).or(i.first_offer_at);
    Lead {
        id,
        channel,
        at,
        name: name.or_else(|| i.customer_name.clone()),
        inquiry_id: Some(i.id),
        stage,
        outcome,
        reason,
        value_cents: i.price_cents,
        days_to_kva: kva_at.map(|k| days_between(at, k)),
        days_to_order: match (sent_at(i), i.accepted_at) {
            (Some(s), Some(a)) => Some(days_between(s, a)),
            _ => None,
        },
    }
}

/// Every lead, all time: submissions claim their inquiry, the rest are phone calls.
pub(crate) fn build_leads(
    inquiries: &[InquiryFactRow],
    forms: &[FormSubmissionRow],
    flash: &[FlashContactRow],
    now: DateTime<Utc>,
) -> Vec<Lead> {
    let people: Vec<Person> = inquiries.iter().map(inquiry_person).collect();
    let by_id: HashMap<Uuid, usize> = inquiries
        .iter()
        .enumerate()
        .map(|(i, q)| (q.id, i))
        .collect();
    let mut claimed: HashMap<usize, usize> = HashMap::new(); // inquiry index → lead index
    let mut leads: Vec<Lead> = Vec::new();
    // Submissions that produced no inquiry yet, for duplicate detection.
    let mut unconverted: Vec<(Person, DateTime<Utc>)> = Vec::new();

    for s in submissions(forms, flash) {
        let direct = s.direct_inquiry.and_then(|id| by_id.get(&id).copied());
        let candidates: Vec<usize> = match direct {
            Some(idx) => vec![idx],
            None => {
                let mut c: Vec<usize> = (0..inquiries.len())
                    .filter(|&i| {
                        let created = inquiries[i].created_at;
                        created >= s.at - MATCH_BEFORE
                            && created <= s.at + MATCH_AFTER
                            && people[i].same_as(&s.person)
                    })
                    .collect();
                c.sort_by_key(|&i| inquiries[i].created_at);
                c
            }
        };
        let free = candidates
            .iter()
            .copied()
            .find(|i| !claimed.contains_key(i));

        if let Some(idx) = free {
            claimed.insert(idx, leads.len());
            leads.push(lead_from_inquiry(
                s.id,
                s.channel,
                s.at,
                s.name.clone(),
                &inquiries[idx],
                now,
            ));
            continue;
        }

        let duplicate = !candidates.is_empty()
            || unconverted
                .iter()
                .any(|(p, at)| s.at - *at <= DUPLICATE_WINDOW && p.same_as(&s.person));
        let (outcome, reason) = if duplicate {
            (Outcome::Duplicate, None)
        } else if s.dismissed || now - s.at > NO_INQUIRY_AFTER {
            (Outcome::Lost, Some(LostReason::KeineAnfrage))
        } else {
            (Outcome::Open, None)
        };
        if !duplicate {
            unconverted.push((s.person.clone(), s.at));
        }
        leads.push(Lead {
            id: s.id,
            channel: s.channel,
            at: s.at,
            name: s.name,
            inquiry_id: candidates.first().map(|&i| inquiries[i].id),
            stage: 0,
            outcome,
            reason,
            value_cents: None,
            days_to_kva: None,
            days_to_order: None,
        });
    }

    for (idx, i) in inquiries.iter().enumerate() {
        if !claimed.contains_key(&idx) {
            let channel = Channel::from_source(i.source.as_deref());
            leads.push(lead_from_inquiry(i.id, channel, i.created_at, None, i, now));
        }
    }

    leads.sort_by(|a, b| b.at.cmp(&a.at).then(a.id.cmp(&b.id)));
    leads
}

// ── Aggregation ─────────────────────────────────────────────────────────────

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

fn stats<'a>(channel: Option<Channel>, leads: impl Iterator<Item = &'a Lead>) -> ChannelStats {
    let mut s = ChannelStats {
        channel,
        leads: 0,
        duplicates: 0,
        reached: [0; STAGE_COUNT],
        won: 0,
        lost: 0,
        open: 0,
        lost_by_reason: BTreeMap::new(),
        won_value_cents: 0,
        avg_won_value_cents: None,
        median_days_to_kva: None,
        median_days_to_order: None,
    };
    let mut to_kva = Vec::new();
    let mut to_order = Vec::new();
    for l in leads {
        if l.outcome == Outcome::Duplicate {
            s.duplicates += 1;
            continue;
        }
        s.leads += 1;
        for r in s.reached.iter_mut().take(l.stage as usize + 1) {
            *r += 1;
        }
        match l.outcome {
            Outcome::Won => {
                s.won += 1;
                s.won_value_cents += l.value_cents.unwrap_or(0);
            }
            Outcome::Lost => {
                s.lost += 1;
                if let Some(r) = l.reason {
                    *s.lost_by_reason.entry(r).or_insert(0) += 1;
                }
            }
            Outcome::Open => s.open += 1,
            Outcome::Duplicate => {}
        }
        if l.stage >= STAGE_SENT {
            to_kva.extend(l.days_to_kva);
        }
        to_order.extend(l.days_to_order);
    }
    if s.won > 0 {
        s.avg_won_value_cents = Some(s.won_value_cents / s.won as i64);
    }
    s.median_days_to_kva = median(to_kva);
    s.median_days_to_order = median(to_order);
    s
}

fn berlin_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&chrono_tz::Europe::Berlin).date_naive()
}

/// The whole tab: funnel per channel, month trend and the lead list for `from..=to`.
pub(crate) fn build_report(
    inquiries: &[InquiryFactRow],
    forms: &[FormSubmissionRow],
    flash: &[FlashContactRow],
    now: DateTime<Utc>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> ConversionReport {
    let leads: Vec<Lead> = build_leads(inquiries, forms, flash, now)
        .into_iter()
        .filter(|l| {
            let d = berlin_date(l.at);
            from.is_none_or(|f| d >= f) && to.is_none_or(|t| d <= t)
        })
        .collect();

    let channels = CHANNELS
        .iter()
        .map(|&c| stats(Some(c), leads.iter().filter(|l| l.channel == c)))
        .filter(|s| s.leads + s.duplicates > 0)
        .collect();

    let mut months: BTreeMap<String, BTreeMap<Channel, (u32, u32)>> = BTreeMap::new();
    for l in leads.iter().filter(|l| l.outcome != Outcome::Duplicate) {
        let e = months
            .entry(berlin_date(l.at).format("%Y-%m").to_string())
            .or_default()
            .entry(l.channel)
            .or_default();
        e.0 += 1;
        if l.outcome == Outcome::Won {
            e.1 += 1;
        }
    }
    let months = months
        .into_iter()
        .map(|(month, by)| MonthRow {
            month,
            channels: by
                .into_iter()
                .map(|(channel, (leads, won))| MonthChannel {
                    channel,
                    leads,
                    won,
                })
                .collect(),
        })
        .collect();

    ConversionReport {
        from,
        to,
        total: stats(None, leads.iter()),
        channels,
        months,
        leads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, day, 10, 0, 0).unwrap()
    }
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap()
    }

    fn inquiry(id: u128, created: DateTime<Utc>, status: &str) -> InquiryFactRow {
        InquiryFactRow {
            id: Uuid::from_u128(id),
            customer_key: None,
            source: Some("admin_dashboard".into()),
            status: status.into(),
            created_at: created,
            offer_sent_at: None,
            accepted_at: None,
            customer_name: Some(format!("Kunde {id}")),
            customer_email: None,
            customer_phone: None,
            first_offer_at: None,
            kva_sent_at: None,
            price_cents: None,
        }
    }

    fn form(id: u128, subject: &str, at: DateTime<Utc>) -> FormSubmissionRow {
        FormSubmissionRow {
            id: Uuid::from_u128(id),
            subject: subject.into(),
            created_at: at,
            customer_key: None,
            inquiry_id: None,
            name: Some("Formular".into()),
            email: None,
            phone: None,
        }
    }

    fn flash(id: u128, phone: &str, at: DateTime<Utc>) -> FlashContactRow {
        FlashContactRow {
            id: Uuid::from_u128(id),
            name: "Rückruf".into(),
            phone: phone.into(),
            created_at: at,
            dismissed_at: None,
        }
    }

    fn lead(leads: &[Lead], id: u128) -> &Lead {
        leads
            .iter()
            .find(|l| l.id == Uuid::from_u128(id))
            .expect("lead")
    }

    #[test]
    fn phone_keys_fold_country_code_and_formatting() {
        assert_eq!(
            phone_key(Some("0151 234 56789")),
            phone_key(Some("+49 151 23456789"))
        );
        assert_eq!(
            phone_key(Some("0049 (151) 234-56789")),
            phone_key(Some("015123456789"))
        );
        assert_eq!(phone_key(Some("12345")), None);
        assert_eq!(phone_key(None), None);
    }

    #[test]
    fn hand_made_inquiry_without_submission_is_a_phone_lead() {
        let leads = build_leads(&[inquiry(1, t(1), "paid")], &[], &[], now());
        let l = lead(&leads, 1);
        assert_eq!(l.channel, Channel::Telefon);
        assert_eq!(l.stage, STAGE_PAID);
        assert_eq!(l.outcome, Outcome::Won);
    }

    #[test]
    fn contact_form_claims_the_inquiry_alex_typed_in_afterwards() {
        let mut q = inquiry(1, t(3), "accepted");
        q.customer_email = Some("Kunde@Example.de".into());
        let mut f = form(10, "Neue Kontaktanfrage von Kunde", t(1));
        f.email = Some("kunde@example.de".into());
        let leads = build_leads(&[q], &[f], &[], now());
        assert_eq!(
            leads.len(),
            1,
            "the inquiry is not counted a second time as Telefon"
        );
        let l = lead(&leads, 10);
        assert_eq!(l.channel, Channel::Kontaktformular);
        assert_eq!(l.inquiry_id, Some(Uuid::from_u128(1)));
        assert_eq!(l.outcome, Outcome::Won);
    }

    #[test]
    fn rueckruf_matches_by_phone_number() {
        let mut q = inquiry(1, t(2), "offer_sent");
        q.customer_phone = Some("+49 151 23456789".into());
        q.kva_sent_at = Some(t(4));
        q.first_offer_at = Some(t(4));
        let leads = build_leads(&[q], &[], &[flash(20, "0151/23456789", t(1))], now());
        let l = lead(&leads, 20);
        assert_eq!(l.channel, Channel::Rueckruf);
        assert_eq!(l.stage, STAGE_SENT);
        // Sent in June, nothing heard by September.
        assert_eq!(l.reason, Some(LostReason::KeineRueckmeldung));
        assert_eq!(l.days_to_kva, Some(3.0));
    }

    #[test]
    fn inquiry_far_after_the_form_is_a_new_lead() {
        let mut q = inquiry(1, t(1) + Duration::days(120), "pending");
        q.customer_key = Some(Uuid::from_u128(99));
        let mut f = form(10, "Neue Kontaktanfrage von X", t(1));
        f.customer_key = Some(Uuid::from_u128(99));
        let leads = build_leads(&[q], &[f], &[], now());
        assert_eq!(lead(&leads, 10).reason, Some(LostReason::KeineAnfrage));
        assert_eq!(lead(&leads, 1).channel, Channel::Telefon);
    }

    #[test]
    fn second_form_from_the_same_person_is_a_duplicate() {
        let mut q = inquiry(1, t(2), "estimated");
        q.customer_key = Some(Uuid::from_u128(99));
        let mut f1 = form(10, "Neues Angebotsanfrage von X", t(1));
        f1.customer_key = Some(Uuid::from_u128(99));
        let mut f2 = form(11, "Neue Kontaktanfrage von X", t(3));
        f2.customer_key = Some(Uuid::from_u128(99));
        let leads = build_leads(&[q], &[f1, f2], &[], now());
        assert_eq!(lead(&leads, 10).channel, Channel::Angebotsformular);
        assert_eq!(lead(&leads, 11).outcome, Outcome::Duplicate);

        // Two forms, no inquiry at all: still one lead.
        let mut a = form(30, "Neue Kontaktanfrage von Y", t(10));
        a.phone = Some("0170 1234567".into());
        let mut b = form(31, "Neue Kontaktanfrage von Y", t(11));
        b.phone = Some("+49 170 1234567".into());
        let leads = build_leads(&[], &[a, b], &[], now());
        assert_eq!(lead(&leads, 30).outcome, Outcome::Lost);
        assert_eq!(lead(&leads, 31).outcome, Outcome::Duplicate);
    }

    #[test]
    fn pipeline_created_inquiry_is_linked_directly() {
        let mut q = inquiry(1, t(1), "rejected");
        q.source = Some("quote_form".into());
        q.first_offer_at = Some(t(1));
        let mut f = form(10, "Neues Angebotsanfrage von X", t(1));
        f.inquiry_id = Some(Uuid::from_u128(1));
        let leads = build_leads(&[q], &[f], &[], now());
        assert_eq!(leads.len(), 1);
        let l = lead(&leads, 10);
        assert_eq!(l.stage, STAGE_SENT, "a rejected KVA was sent");
        assert_eq!(l.reason, Some(LostReason::KvaAbgelehnt));
    }

    #[test]
    fn drop_off_reasons() {
        let mut cancelled_after = inquiry(1, t(1), "cancelled");
        cancelled_after.accepted_at = Some(t(5));
        let rejected_early = inquiry(2, t(1), "rejected");
        let stale = inquiry(3, t(1), "pending");
        let fresh = inquiry(4, now() - Duration::days(3), "pending");
        let mut expired = inquiry(5, t(1), "expired");
        expired.first_offer_at = Some(t(2));
        let mut rejected_after = inquiry(6, t(1), "rejected");
        rejected_after.accepted_at = Some(t(3));
        let leads = build_leads(
            &[
                cancelled_after,
                rejected_early,
                stale,
                fresh,
                expired,
                rejected_after,
            ],
            &[],
            &[],
            now(),
        );
        assert_eq!(
            lead(&leads, 1).reason,
            Some(LostReason::StorniertNachZusage)
        );
        assert_eq!(lead(&leads, 1).stage, STAGE_ORDER);
        assert_eq!(lead(&leads, 2).reason, Some(LostReason::AbgesagtVorKva));
        assert_eq!(lead(&leads, 3).reason, Some(LostReason::KeinKva));
        assert_eq!(lead(&leads, 4).outcome, Outcome::Open);
        assert_eq!(lead(&leads, 5).reason, Some(LostReason::KvaAbgelaufen));
        assert_eq!(
            lead(&leads, 6).outcome,
            Outcome::Lost,
            "a Zusage later rejected is not an order"
        );
        assert_eq!(
            lead(&leads, 6).reason,
            Some(LostReason::StorniertNachZusage)
        );
    }

    #[test]
    fn report_counts_funnel_and_filters_by_date() {
        let mut won = inquiry(1, t(1), "completed");
        won.price_cents = Some(100_000);
        won.first_offer_at = Some(t(2));
        won.kva_sent_at = Some(t(3));
        won.accepted_at = Some(t(5));
        let lost = inquiry(2, t(2), "rejected");
        let outside = inquiry(
            3,
            Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap(),
            "paid",
        );
        let unanswered_form = form(10, "Neue Kontaktanfrage von Z", t(4));
        let r = build_report(
            &[won, lost, outside],
            &[unanswered_form],
            &[],
            now(),
            NaiveDate::from_ymd_opt(2026, 6, 1),
            NaiveDate::from_ymd_opt(2026, 6, 30),
        );
        assert_eq!(r.total.leads, 3);
        assert_eq!(r.total.reached, [3, 2, 1, 1, 1, 1, 0]);
        assert_eq!(r.total.won, 1);
        assert_eq!(r.total.won_value_cents, 100_000);
        assert_eq!(r.total.median_days_to_order, Some(2.0));
        let tel = r
            .channels
            .iter()
            .find(|c| c.channel == Some(Channel::Telefon))
            .unwrap();
        assert_eq!((tel.leads, tel.won, tel.lost), (2, 1, 1));
        let kontakt = r
            .channels
            .iter()
            .find(|c| c.channel == Some(Channel::Kontaktformular))
            .unwrap();
        assert_eq!(
            kontakt.lost_by_reason.get(&LostReason::KeineAnfrage),
            Some(&1)
        );
        assert_eq!(r.months.len(), 1);
        assert_eq!(r.months[0].month, "2026-06");
    }

    /// Runs the report over a JSON dump of the three queries — a manual check
    /// against real data: `CONVERSION_DUMP=dir cargo test -p aust-api report_on_dump -- --ignored`.
    #[test]
    #[ignore]
    fn report_on_dump() {
        let dir = std::env::var("CONVERSION_DUMP").expect("CONVERSION_DUMP");
        let read = |f: &str| std::fs::read_to_string(format!("{dir}/{f}")).unwrap();
        let inquiries: Vec<InquiryFactRow> = serde_json::from_str(&read("inquiries.json")).unwrap();
        let forms: Vec<FormSubmissionRow> = serde_json::from_str(&read("forms.json")).unwrap();
        let flash: Vec<FlashContactRow> = serde_json::from_str(&read("flash.json")).unwrap();
        let r = build_report(&inquiries, &forms, &flash, Utc::now(), None, None);
        std::fs::write(
            format!("{dir}/report.json"),
            serde_json::to_string_pretty(&r).unwrap(),
        )
        .unwrap();
    }
}
