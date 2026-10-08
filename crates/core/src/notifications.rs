//! Telegram notification kinds and the office's mute switches.
//!
//! Every informational Telegram message names its [`NotificationKind`] and carries
//! a "🔕 Stumm schalten" button ([`mute_keyboard`]). Pressing it stores the kind in
//! `telegram_muted_notifications`; every sender checks [`is_muted`] first.
//! `/benachrichtigungen` lists all kinds with on/off buttons ([`settings_keyboard`]).
//!
//! Not every Telegram message is a notification: approval prompts (KVA, email
//! draft, Lagerungsrechnung, Josie's pending actions), error alerts and reminders
//! Alex set himself have no kind and cannot be muted — silencing them would break
//! a workflow or hide a failure.

use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::warn;

/// A mutable kind of Telegram notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationKind {
    /// "📩 Neue E-Mail eingegangen" (email agent).
    EmailIncoming,
    /// "E-Mail gesendet an …" after a draft was approved.
    EmailSent,
    /// "📥 Neue Anfrage von …".
    InquiryCreated,
    /// KVA ready / sent.
    Offer,
    /// Customer accepted / rejected the KVA in the app.
    CustomerResponse,
    /// "🔄 Name: alt → neu" for important status changes.
    StatusChange,
    /// Invoice issued / overdue events.
    Invoice,
    /// Recurring nag for an unanswered email.
    ReminderEmail,
    /// Recurring nag for a due Zahlungserinnerung / Mahnung.
    ReminderPayment,
    /// Recurring nag for a due Bewertungsanfrage.
    ReminderReview,
    /// "KVA nachfassen" follow-up.
    KvaFollowup,
    /// Vehicle deadlines (TÜV, Inspektion, …).
    Vehicle,
    /// A worker logged their hours.
    HoursLogged,
    /// Rückrufwunsch from the website form.
    Callback,
    /// Daily briefing at 07:00 and 15:00.
    Briefing,
    /// 21:00 heads-up for tomorrow's early appointments.
    EveningPreview,
    /// "n ausstehende Aktion(en) sind abgelaufen".
    ActionExpired,
    /// "🟢 E-Mail-Agent gestartet" after every restart / deploy.
    AgentStarted,
}

impl NotificationKind {
    /// Every kind, in the order `/benachrichtigungen` lists them.
    pub const ALL: &'static [NotificationKind] = &[
        Self::Briefing,
        Self::EveningPreview,
        Self::EmailIncoming,
        Self::EmailSent,
        Self::InquiryCreated,
        Self::Offer,
        Self::CustomerResponse,
        Self::StatusChange,
        Self::Invoice,
        Self::ReminderEmail,
        Self::ReminderPayment,
        Self::ReminderReview,
        Self::KvaFollowup,
        Self::Vehicle,
        Self::HoursLogged,
        Self::Callback,
        Self::ActionExpired,
        Self::AgentStarted,
    ];

    /// Stable key stored in the DB and used in callback data. Never rename.
    pub fn key(self) -> &'static str {
        match self {
            Self::EmailIncoming => "email_neu",
            Self::EmailSent => "email_gesendet",
            Self::InquiryCreated => "anfrage_neu",
            Self::Offer => "angebot",
            Self::CustomerResponse => "kunde_antwort",
            Self::StatusChange => "status",
            Self::Invoice => "rechnung",
            Self::ReminderEmail => "erinnerung_email",
            Self::ReminderPayment => "erinnerung_zahlung",
            Self::ReminderReview => "erinnerung_bewertung",
            Self::KvaFollowup => "kva_nachfassen",
            Self::Vehicle => "fahrzeug",
            Self::HoursLogged => "stunden",
            Self::Callback => "rueckruf",
            Self::Briefing => "briefing",
            Self::EveningPreview => "abendvorschau",
            Self::ActionExpired => "aktion_abgelaufen",
            Self::AgentStarted => "systemstart",
        }
    }

    /// German label shown in Telegram.
    pub fn label(self) -> &'static str {
        match self {
            Self::EmailIncoming => "Neue E-Mails",
            Self::EmailSent => "E-Mail-Versandbestätigung",
            Self::InquiryCreated => "Neue Anfragen",
            Self::Offer => "Angebote (fertig / verschickt)",
            Self::CustomerResponse => "Kunde nimmt Angebot an / lehnt ab",
            Self::StatusChange => "Statuswechsel",
            Self::Invoice => "Rechnungen (erstellt / überfällig)",
            Self::ReminderEmail => "Erinnerung: unbeantwortete E-Mails",
            Self::ReminderPayment => "Erinnerung: Zahlungen & Mahnungen",
            Self::ReminderReview => "Erinnerung: Bewertungsanfragen",
            Self::KvaFollowup => "KVA nachfassen",
            Self::Vehicle => "Fahrzeug-Fristen",
            Self::HoursLogged => "Stundenmeldungen",
            Self::Callback => "Rückrufwünsche",
            Self::Briefing => "Tagesübersicht (07:00 / 15:00)",
            Self::EveningPreview => "Abendvorschau Frühtermine (21:00)",
            Self::ActionExpired => "Abgelaufene Aktionen",
            Self::AgentStarted => "Systemstart-Meldung",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.key() == key)
    }

    /// The kind of an auto-nag in `agent_reminders`, by its `source`. Reminders
    /// Alex set himself (`source` NULL or anything else) have none.
    pub fn for_reminder_source(source: Option<&str>) -> Option<Self> {
        match source? {
            "email" => Some(Self::ReminderEmail),
            "invoice" => Some(Self::ReminderPayment),
            "review" => Some(Self::ReminderReview),
            _ => None,
        }
    }
}

/// Whether the running tenant muted `kind`. Fails open: a DB error sends the
/// notification rather than swallowing it.
pub async fn is_muted(pool: &PgPool, kind: NotificationKind) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM telegram_muted_notifications WHERE kind = $1)",
    )
    .bind(kind.key())
    .fetch_one(pool)
    .await
    {
        Ok(muted) => muted,
        Err(e) => {
            warn!(kind = kind.key(), "Mute lookup failed, sending anyway: {e}");
            false
        }
    }
}

/// Switch `kind` off (`muted = true`) or back on for the running tenant.
pub async fn set_muted(pool: &PgPool, kind: NotificationKind, muted: bool) -> Result<(), sqlx::Error> {
    let q = if muted {
        "INSERT INTO telegram_muted_notifications (kind) VALUES ($1) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM telegram_muted_notifications WHERE kind = $1"
    };
    sqlx::query(q).bind(kind.key()).execute(pool).await?;
    Ok(())
}

/// Keys of all muted kinds for the running tenant.
pub async fn muted_kinds(pool: &PgPool) -> Result<Vec<NotificationKind>, sqlx::Error> {
    let keys: Vec<String> = sqlx::query_scalar("SELECT kind FROM telegram_muted_notifications")
        .fetch_all(pool)
        .await?;
    Ok(keys.iter().filter_map(|k| NotificationKind::from_key(k)).collect())
}

/// Callback-data prefixes. `nmute`/`nunmute` sit under a single notification;
/// `nset` toggles a row in the `/benachrichtigungen` list.
pub const CB_MUTE: &str = "nmute";
pub const CB_UNMUTE: &str = "nunmute";
pub const CB_SETTINGS_TOGGLE: &str = "nset";

/// A notification-related button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteCallback {
    /// Under a notification: mute (`true`) or unmute (`false`) its kind.
    Single(NotificationKind, bool),
    /// In the settings list: flip this kind.
    SettingsToggle(NotificationKind),
}

/// Parse `nmute:<kind>` / `nunmute:<kind>` / `nset:<kind>`.
pub fn parse_callback(data: &str) -> Option<MuteCallback> {
    let (action, key) = data.split_once(':')?;
    let kind = NotificationKind::from_key(key)?;
    match action {
        CB_MUTE => Some(MuteCallback::Single(kind, true)),
        CB_UNMUTE => Some(MuteCallback::Single(kind, false)),
        CB_SETTINGS_TOGGLE => Some(MuteCallback::SettingsToggle(kind)),
        _ => None,
    }
}

/// The inline keyboard under a notification: one "Stumm schalten" button.
pub fn mute_keyboard(kind: NotificationKind) -> Value {
    json!({ "inline_keyboard": [[
        { "text": "🔕 Stumm schalten", "callback_data": format!("{CB_MUTE}:{}", kind.key()) }
    ]]})
}

/// The keyboard that replaces [`mute_keyboard`] once the kind is muted.
pub fn unmute_keyboard(kind: NotificationKind) -> Value {
    json!({ "inline_keyboard": [[
        { "text": format!("🔔 Wieder einschalten ({})", kind.label()),
          "callback_data": format!("{CB_UNMUTE}:{}", kind.key()) }
    ]]})
}

/// Text + keyboard for `/benachrichtigungen`: one toggle row per kind.
pub fn settings_message(muted: &[NotificationKind]) -> (String, Value) {
    let rows: Vec<Value> = NotificationKind::ALL
        .iter()
        .map(|k| {
            let icon = if muted.contains(k) { "🔕" } else { "🔔" };
            json!([{
                "text": format!("{icon} {}", k.label()),
                "callback_data": format!("{CB_SETTINGS_TOGGLE}:{}", k.key()),
            }])
        })
        .collect();
    let text = "Benachrichtigungen\n\n🔔 = an, 🔕 = stumm. Tippen schaltet um.\n\
                Freigaben (KVA, E-Mail-Entwürfe), Fehlermeldungen und selbst gesetzte \
                Erinnerungen kommen immer."
        .to_string();
    (text, json!({ "inline_keyboard": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for k in NotificationKind::ALL {
            assert!(seen.insert(k.key()), "duplicate key {}", k.key());
            assert_eq!(NotificationKind::from_key(k.key()), Some(*k));
        }
    }

    #[test]
    fn callback_data_fits_telegram_limit() {
        for k in NotificationKind::ALL {
            assert!(format!("{CB_UNMUTE}:{}", k.key()).len() <= 64);
        }
    }

    #[test]
    fn parses_mute_callbacks() {
        assert_eq!(
            parse_callback("nmute:email_neu"),
            Some(MuteCallback::Single(NotificationKind::EmailIncoming, true))
        );
        assert_eq!(
            parse_callback("nunmute:briefing"),
            Some(MuteCallback::Single(NotificationKind::Briefing, false))
        );
        assert_eq!(
            parse_callback("nset:stunden"),
            Some(MuteCallback::SettingsToggle(NotificationKind::HoursLogged))
        );
        assert_eq!(parse_callback("nmute:unknown"), None);
        assert_eq!(parse_callback("approve:123"), None);
    }

    #[test]
    fn user_reminders_have_no_kind() {
        assert_eq!(NotificationKind::for_reminder_source(None), None);
        assert_eq!(NotificationKind::for_reminder_source(Some("user")), None);
        assert_eq!(
            NotificationKind::for_reminder_source(Some("invoice")),
            Some(NotificationKind::ReminderPayment)
        );
    }

    #[test]
    fn settings_marks_muted_kinds() {
        let (_, kb) = settings_message(&[NotificationKind::Briefing]);
        let rows = kb["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows.len(), NotificationKind::ALL.len());
        assert!(rows[0][0]["text"].as_str().unwrap().starts_with("🔕"));
        assert!(rows[1][0]["text"].as_str().unwrap().starts_with("🔔"));
    }
}
