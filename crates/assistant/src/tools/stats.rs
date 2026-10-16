//! Statistics tools: invoice totals and the conversion funnel.
//!
//! Money comes from the Rechnungsausgangsbuch rows (`MetricsService::invoice_register`)
//! with the register's own amounts and "counts in the totals" rule, so whatever
//! Josie adds up matches the admin register and the Gewinn tab to the cent.
//! Aggregation is pure ([`aggregate`]) and tested without a database.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};

use aust_core::services::RegisterInvoice;

use crate::error::Result;
use crate::roles::Role;
use super::{parse_date_opt, Safety, Tool, ToolCtx};

/// Rows listed individually at most; totals always cover everything.
const MAX_ROWS: usize = 25;

/// `1234567` cents → `"12.345,67 €"`.
pub(crate) fn eur(cents: i64) -> String {
    let neg = cents < 0;
    let c = cents.unsigned_abs();
    let euros = (c / 100).to_string();
    let mut grouped = String::new();
    for (i, ch) in euros.chars().enumerate() {
        if i > 0 && (euros.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(ch);
    }
    format!("{}{},{:02} €", if neg { "-" } else { "" }, grouped, c % 100)
}

/// Lowercase with umlauts folded, for name matching ("Lüttert" ≈ "luttert").
fn fold(s: &str) -> String {
    s.to_lowercase()
        .replace('ä', "a")
        .replace('ö', "o")
        .replace('ü', "u")
        .replace('ß', "ss")
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Does `name` match the customer search? Substring on the folded name, or one
/// word within edit distance 2 — voice notes hear "Lüthert" for "Luttert".
fn name_matches(name: &str, query: &str, fuzzy: bool) -> bool {
    let (n, q) = (fold(name), fold(query.trim()));
    if n.contains(&q) {
        return true;
    }
    fuzzy
        && q.chars().count() >= 4
        && n.split(|c: char| !c.is_alphanumeric())
            .any(|w| w.chars().count() >= 4 && levenshtein(w, &q) <= 2)
}

/// Date a row is filed under: its Leistungsdatum, else its Rechnungsdatum.
fn row_date(r: &RegisterInvoice) -> Option<NaiveDate> {
    r.service_start.or(r.issued_on)
}

fn status_de(r: &RegisterInvoice) -> &'static str {
    match r.open_cents {
        Some(o) if o > 0 && r.paid_on.is_none() && r.brutto_cents.is_some_and(|b| o < b) => "teilbezahlt",
        Some(o) if o > 0 => "offen",
        _ if r.paid_on.is_some() || r.status == "paid" => "bezahlt",
        Some(_) => "bezahlt",
        None => "Betrag unbekannt",
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Filter {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub customer: Option<String>,
    /// `all` | `open` | `paid`
    pub status: String,
    /// `none` | `customer` | `month` | `quarter` | `year` | `status`
    pub group_by: String,
}

#[derive(Default)]
struct Sum {
    count: u32,
    netto: i64,
    brutto: i64,
    open: i64,
    unknown: u32,
}

impl Sum {
    fn add(&mut self, r: &RegisterInvoice) {
        self.count += 1;
        match (r.netto_cents, r.brutto_cents) {
            (Some(n), Some(b)) => {
                self.netto += n;
                self.brutto += b;
            }
            _ => self.unknown += 1,
        }
        self.open += r.open_cents.filter(|o| *o > 0).unwrap_or(0);
    }

    fn json(&self) -> Value {
        let mut v = json!({
            "anzahl": self.count,
            "netto": eur(self.netto),
            "brutto": eur(self.brutto),
            "offen_brutto": eur(self.open),
            "netto_cents": self.netto,
            "brutto_cents": self.brutto,
            "offen_cents": self.open,
        });
        if self.unknown > 0 {
            v["ohne_betrag"] = json!(self.unknown);
        }
        v
    }
}

fn group_key(r: &RegisterInvoice, by: &str) -> String {
    let d = row_date(r);
    match by {
        "customer" => r.customer_name.clone().unwrap_or_else(|| "(ohne Kunde)".into()),
        "month" => d.map(|d| d.format("%Y-%m").to_string()).unwrap_or_else(|| "(ohne Datum)".into()),
        "quarter" => d
            .map(|d| format!("{} Q{}", d.year(), (d.month() - 1) / 3 + 1))
            .unwrap_or_else(|| "(ohne Datum)".into()),
        "year" => d.map(|d| d.year().to_string()).unwrap_or_else(|| "(ohne Datum)".into()),
        "status" => status_de(r).to_string(),
        _ => String::new(),
    }
}

/// Totals, groups and the matching invoices for `filter`.
pub(crate) fn aggregate(rows: &[RegisterInvoice], f: &Filter) -> Value {
    let in_range = |r: &&RegisterInvoice| {
        let d = row_date(r);
        f.from.is_none_or(|from| d.is_some_and(|d| d >= from))
            && f.to.is_none_or(|to| d.is_some_and(|d| d <= to))
    };
    let counted: Vec<&RegisterInvoice> = rows.iter().filter(|r| r.counts).filter(in_range).collect();
    let not_counted = rows.iter().filter(|r| !r.counts).filter(in_range).count();

    // Exact name match first; only when nothing matches, the fuzzy one.
    let mut fuzzy_used = false;
    let by_customer: Vec<&RegisterInvoice> = match f.customer.as_deref().filter(|c| !c.trim().is_empty()) {
        None => counted,
        Some(q) => {
            let exact: Vec<_> = counted
                .iter()
                .copied()
                .filter(|r| r.customer_name.as_deref().is_some_and(|n| name_matches(n, q, false)))
                .collect();
            if exact.is_empty() {
                fuzzy_used = true;
                counted
                    .into_iter()
                    .filter(|r| r.customer_name.as_deref().is_some_and(|n| name_matches(n, q, true)))
                    .collect()
            } else {
                exact
            }
        }
    };
    let selected: Vec<&RegisterInvoice> = by_customer
        .into_iter()
        .filter(|r| match f.status.as_str() {
            "open" => r.open_cents.is_some_and(|o| o > 0),
            "paid" => r.open_cents.is_some_and(|o| o <= 0),
            _ => true,
        })
        .collect();

    let mut total = Sum::default();
    for r in &selected {
        total.add(r);
    }

    let mut out = json!({
        "zeitraum": {
            "von": f.from.map(|d| d.format("%d.%m.%Y").to_string()),
            "bis": f.to.map(|d| d.format("%d.%m.%Y").to_string()),
            "nach": "Leistungsdatum (sonst Rechnungsdatum)",
        },
        "summe": total.json(),
    });

    if let Some(q) = f.customer.as_deref() {
        let mut names: Vec<&str> = selected.iter().filter_map(|r| r.customer_name.as_deref()).collect();
        names.sort_unstable();
        names.dedup();
        out["kunden_gefunden"] = json!(names);
        if fuzzy_used && !names.is_empty() {
            out["hinweis"] = json!(format!(
                "Kein Kunde enthält „{q}“ — ähnlich geschriebene Namen verwendet. Nenne Alex den gefundenen Namen."
            ));
        }
    }

    if !matches!(f.group_by.as_str(), "" | "none") {
        let mut groups: BTreeMap<String, Sum> = BTreeMap::new();
        for r in &selected {
            groups.entry(group_key(r, &f.group_by)).or_default().add(r);
        }
        let mut list: Vec<(String, Sum)> = groups.into_iter().collect();
        if f.group_by == "customer" || f.group_by == "status" {
            list.sort_by(|a, b| b.1.netto.cmp(&a.1.netto));
        }
        let shown: Vec<Value> = list
            .iter()
            .take(MAX_ROWS)
            .map(|(k, s)| {
                let mut v = s.json();
                v["gruppe"] = json!(k);
                v
            })
            .collect();
        out["gruppen"] = json!(shown);
        if list.len() > MAX_ROWS {
            out["weitere_gruppen"] = json!(list.len() - MAX_ROWS);
        }
    }

    let mut newest: Vec<&&RegisterInvoice> = selected.iter().collect();
    newest.sort_by(|a, b| row_date(b).cmp(&row_date(a)));
    out["rechnungen"] = json!(
        newest
            .iter()
            .take(MAX_ROWS)
            .map(|r| invoice_json(r))
            .collect::<Vec<_>>()
    );
    if selected.len() > MAX_ROWS {
        out["weitere_rechnungen"] = json!(selected.len() - MAX_ROWS);
    }
    if not_counted > 0 {
        out["nicht_gezählt"] = json!(format!(
            "{not_counted} Entwürfe/stornierte Rechnungen im Zeitraum sind nicht enthalten"
        ));
    }
    out
}

fn invoice_json(r: &RegisterInvoice) -> Value {
    let leistung = match (r.service_start, r.service_end) {
        (Some(a), Some(b)) if a != b => format!("{}–{}", a.format("%d.%m."), b.format("%d.%m.%Y")),
        (Some(a), _) => a.format("%d.%m.%Y").to_string(),
        _ => String::new(),
    };
    let mut v = json!({
        "nr": r.invoice_number,
        "kunde": r.customer_name,
        "leistung": leistung,
        "netto": r.netto_cents.map(eur),
        "brutto": r.brutto_cents.map(eur),
        "status": status_de(r),
        "invoice_id": r.id,
    });
    if let Some(o) = r.open_cents.filter(|o| *o > 0) {
        v["offen"] = json!(eur(o));
    }
    if let Some(p) = r.paid_on {
        v["bezahlt_am"] = json!(p.format("%d.%m.%Y").to_string());
    }
    if r.invoice_type == "partial_first" {
        v["art"] = json!("Anzahlung");
    } else if r.invoice_type == "partial_final" {
        v["art"] = json!("Schlussrechnung");
    } else if r.invoice_type == "lagerung" {
        v["art"] = json!("Lagerung");
    }
    v
}

fn filter_from_args(args: &Value, tool: &str) -> Result<Filter> {
    let mut from = parse_date_opt(args, "from", tool)?;
    let mut to = parse_date_opt(args, "to", tool)?;
    if let Some(y) = args["year"].as_i64().and_then(|y| i32::try_from(y).ok()) {
        from = from.or(NaiveDate::from_ymd_opt(y, 1, 1));
        to = to.or(NaiveDate::from_ymd_opt(y, 12, 31));
    }
    Ok(Filter {
        from,
        to,
        customer: args["customer"].as_str().map(str::to_string),
        status: args["status"].as_str().unwrap_or("all").to_string(),
        group_by: args["group_by"].as_str().unwrap_or("none").to_string(),
    })
}

// ── InvoiceStats ──────────────────────────────────────────────────────────────

pub struct InvoiceStats;

#[async_trait]
impl Tool for InvoiceStats {
    fn name(&self) -> &'static str { "invoice_stats" }
    fn description(&self) -> &'static str {
        "Umsatz- und Rechnungsstatistik aus dem Rechnungsausgangsbuch — exakt die Beträge des \
         Dashboards. Für alle Fragen nach Umsatz, Auftragsvolumen in €, „wie viel hat Kunde X \
         bezahlt“, offenen Beträgen, Monats-/Quartals-/Jahressummen, Top-Kunden. Filter: Zeitraum \
         (from/to oder year, nach Leistungsdatum), customer (Name, auch ungenau), status \
         (all|open|paid). group_by: none|customer|month|quarter|year|status. Liefert Summen \
         (netto, brutto, offen), Gruppen und bis zu 25 Rechnungen. Entwürfe und Stornos zählen \
         nicht. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "format": "date", "description": "Leistungsdatum ab (YYYY-MM-DD)" },
                "to": { "type": "string", "format": "date", "description": "Leistungsdatum bis (YYYY-MM-DD)" },
                "year": { "type": "integer", "description": "Kurzform für ein ganzes Jahr, z. B. 2026" },
                "customer": { "type": "string", "description": "Kundenname oder Teil davon" },
                "status": { "type": "string", "enum": ["all", "open", "paid"] },
                "group_by": { "type": "string", "enum": ["none", "customer", "month", "quarter", "year", "status"] }
            }
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let filter = filter_from_args(args, self.name())?;
        let rows = ctx.services.metrics.invoice_register().await?;
        Ok(aggregate(&rows, &filter))
    }
}

// ── ConversionStats ───────────────────────────────────────────────────────────

pub struct ConversionStats;

#[async_trait]
impl Tool for ConversionStats {
    fn name(&self) -> &'static str { "conversion_stats" }
    fn description(&self) -> &'static str {
        "Conversion-Auswertung wie im Dashboard-Tab „Conversion“: Eingänge je Kanal (Telefon, \
         Kontaktformular, Angebotsformular, Rückruf, E-Mail, App), wie weit sie kamen \
         (reached = Anzahl je Stufe: 0 Eingang, 1 Anfrage, 2 KVA erstellt, 3 KVA versendet, \
         4 Auftrag, 5 Durchgeführt, 6 Bezahlt), gewonnen/verloren/offen, Absprunggründe, \
         Auftragswert netto (Cent), Median-Tage bis KVA und bis Zusage, Monatsverlauf. \
         Zeitraum nach Eingangsdatum. Nur für Inhaber."
    }
    fn params_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "format": "date" },
                "to": { "type": "string", "format": "date" },
                "year": { "type": "integer" }
            }
        })
    }
    fn safety(&self) -> Safety { Safety::Read }
    fn min_role(&self) -> Role { Role::Owner }

    async fn execute(&self, ctx: &ToolCtx, args: &Value) -> Result<Value> {
        let f = filter_from_args(args, self.name())?;
        let mut report = ctx.services.metrics.conversion_report(f.from, f.to).await?;
        // The per-lead list runs to hundreds of rows; the model needs the totals.
        if let Some(obj) = report.as_object_mut()
            && let Some(leads) = obj.remove("leads")
        {
            obj.insert("leads_count".into(), json!(leads.as_array().map_or(0, Vec::len)));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn row(nr: &str, customer: &str, day: &str, netto: i64, open: i64, counts: bool) -> RegisterInvoice {
        let d: NaiveDate = day.parse().unwrap();
        RegisterInvoice {
            id: Uuid::now_v7(),
            invoice_number: nr.into(),
            inquiry_id: None,
            customer_name: Some(customer.into()),
            service_start: Some(d),
            service_end: Some(d),
            issued_on: Some(d),
            netto_cents: Some(netto),
            brutto_cents: Some(netto * 119 / 100),
            open_cents: Some(open),
            paid_on: (open == 0).then_some(d),
            status: if open == 0 { "paid".into() } else { "sent".into() },
            invoice_type: "full".into(),
            counts,
        }
    }

    fn rows() -> Vec<RegisterInvoice> {
        vec![
            row("2026-01", "Sebastian Luttert", "2026-01-20", 148_800, 0, true),
            row("2026-12", "Sebastian Luttert", "2026-03-18", 262_200, 0, true),
            row("2026-96", "Sebastian Luttert", "2026-10-01", 39_900, 47_481, false),
            row("2026-40", "Frau Lange", "2026-05-04", 100_000, 119_000, true),
            row("2025-90", "Frau Lange", "2025-12-01", 50_000, 0, true),
        ]
    }

    #[test]
    fn euro_formatting() {
        assert_eq!(eur(123_456_78), "123.456,78 €");
        assert_eq!(eur(5), "0,05 €");
        assert_eq!(eur(-100_000), "-1.000,00 €");
    }

    #[test]
    fn customer_year_total_matches_the_register() {
        let f = Filter { customer: Some("luttert".into()), from: "2026-01-01".parse().ok(), to: "2026-12-31".parse().ok(), status: "all".into(), ..Default::default() };
        let v = aggregate(&rows(), &f);
        assert_eq!(v["summe"]["anzahl"], 2, "the draft 2026-96 does not count");
        assert_eq!(v["summe"]["netto_cents"], 411_000);
        assert_eq!(v["summe"]["netto"], "4.110,00 €");
        assert_eq!(v["nicht_gezählt"].as_str().unwrap().starts_with('1'), true);
        assert!(v.get("hinweis").is_none());
    }

    #[test]
    fn misheard_name_falls_back_to_fuzzy_match() {
        let f = Filter { customer: Some("Lüthert".into()), status: "all".into(), ..Default::default() };
        let v = aggregate(&rows(), &f);
        assert_eq!(v["summe"]["anzahl"], 2);
        assert_eq!(v["kunden_gefunden"], json!(["Sebastian Luttert"]));
        assert!(v["hinweis"].as_str().unwrap().contains("Lüthert"));
    }

    #[test]
    fn open_invoices_grouped_by_customer() {
        let f = Filter { status: "open".into(), group_by: "customer".into(), ..Default::default() };
        let v = aggregate(&rows(), &f);
        assert_eq!(v["summe"]["anzahl"], 1);
        assert_eq!(v["summe"]["offen_brutto"], "1.190,00 €");
        assert_eq!(v["gruppen"][0]["gruppe"], "Frau Lange");
        assert_eq!(v["rechnungen"][0]["status"], "offen");
    }

    #[test]
    fn grouping_by_month_and_year() {
        let f = Filter { group_by: "year".into(), status: "all".into(), ..Default::default() };
        let v = aggregate(&rows(), &f);
        let groups = v["gruppen"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0]["gruppe"], "2025");
        assert_eq!(groups[1]["netto_cents"], 511_000);
        let f = Filter { group_by: "quarter".into(), status: "all".into(), ..Default::default() };
        assert_eq!(aggregate(&rows(), &f)["gruppen"][1]["gruppe"], "2026 Q1");
    }

    #[test]
    fn short_queries_never_match_fuzzily() {
        assert!(!name_matches("Sebastian Luttert", "Lutz", true));
        assert!(name_matches("Jörg Müller", "muller", false));
    }
}
