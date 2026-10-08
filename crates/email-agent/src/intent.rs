//! What an incoming mail wants, decided before any reply is drafted.
//!
//! Most mail is a reply to a KVA ("Re: Ihr Umzugsangebot") from someone the system
//! already knows. Only a genuinely new request goes through the intake flow that
//! asks for moving details; everything else gets a reply grounded in the
//! customer's real data, or no draft at all.

use serde::Deserialize;

/// The kind of mail, as far as the reply is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailIntent {
    /// Someone without an open job asks for a quote / a move / a clearance.
    NewInquiry,
    /// Details for an inquiry that is still being collected (addresses, list, photos).
    InquiryDetails,
    /// A question about an existing job, the offer, or the company.
    Question,
    /// Accepts the offer / books the job.
    AcceptOffer,
    /// Declines the offer.
    RejectOffer,
    /// Wants a different date or time.
    Reschedule,
    /// Cancels a booked job.
    Cancellation,
    /// About an invoice or a payment.
    Payment,
    /// Unhappy about something.
    Complaint,
    /// Just says thanks / confirms — nothing to answer.
    ThanksOnly,
    /// Bounce, out-of-office, newsletter, other machine mail.
    Automated,
    /// None of the above.
    Other,
}

impl MailIntent {
    fn from_key(k: &str) -> Option<Self> {
        Some(match k {
            "neue_anfrage" => Self::NewInquiry,
            "anfrage_details" => Self::InquiryDetails,
            "frage" => Self::Question,
            "annahme" => Self::AcceptOffer,
            "ablehnung" => Self::RejectOffer,
            "terminaenderung" => Self::Reschedule,
            "stornierung" => Self::Cancellation,
            "zahlung" => Self::Payment,
            "beschwerde" => Self::Complaint,
            "dank" => Self::ThanksOnly,
            "automatisch" => Self::Automated,
            "sonstiges" => Self::Other,
            _ => return None,
        })
    }

    /// German label for Telegram.
    pub fn label(self) -> &'static str {
        match self {
            Self::NewInquiry => "Neue Anfrage",
            Self::InquiryDetails => "Angaben zur Anfrage",
            Self::Question => "Frage",
            Self::AcceptOffer => "Nimmt Angebot an",
            Self::RejectOffer => "Lehnt Angebot ab",
            Self::Reschedule => "Möchte Termin ändern",
            Self::Cancellation => "Storniert",
            Self::Payment => "Rechnung / Zahlung",
            Self::Complaint => "Beschwerde",
            Self::ThanksOnly => "Nur Dank / Bestätigung",
            Self::Automated => "Automatische Mail",
            Self::Other => "Sonstiges",
        }
    }

    /// Whether this mail goes through the intake flow (collect moving details,
    /// generate a KVA once complete).
    pub fn is_intake(self) -> bool {
        matches!(self, Self::NewInquiry | Self::InquiryDetails)
    }
}

/// The classifier's verdict.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Classification {
    pub intent: MailIntent,
    /// Whether the customer is waiting for an answer from us.
    pub needs_reply: bool,
    /// One German sentence for Alex: what the mail is about.
    pub summary: String,
}

/// Mail no reply should ever be drafted for, recognisable without an LLM.
pub(crate) fn is_automated(from: &str, subject: &str, body: &str) -> bool {
    let from = from.to_lowercase();
    let subject = subject.to_lowercase();
    let local = from.split('@').next().unwrap_or("");
    if ["mailer-daemon", "postmaster", "no-reply", "noreply", "do-not-reply", "donotreply"]
        .iter()
        .any(|p| local.contains(p))
    {
        return true;
    }
    const SUBJECTS: &[&str] = &[
        "undelivered mail",
        "delivery status notification",
        "mail delivery failed",
        "returned mail",
        "unzustellbar",
        "nicht zugestellt",
        "automatische antwort",
        "automatic reply",
        "auto-reply",
        "autoreply",
        "out of office",
        "abwesenheitsnotiz",
        "abwesenheit:",
    ];
    if SUBJECTS.iter().any(|s| subject.contains(s)) {
        return true;
    }
    let head: String = body.chars().take(400).collect::<String>().to_lowercase();
    head.contains("this is the mail system at host") || head.contains("abwesenheitsnotiz")
}

pub(crate) const SYSTEM_PROMPT: &str = r#"Du ordnest eingehende E-Mails eines Umzugs- und Entrümpelungsunternehmens ein.
Du bekommst die E-Mail und alles, was wir über den Absender wissen (Aufträge, Angebote, Rechnungen, bisheriger Verlauf).

Wähle genau eine Kategorie:
- neue_anfrage: jemand OHNE offenen Auftrag möchte ein Angebot, einen Umzug, eine Entrümpelung o.ä. anfragen
- anfrage_details: liefert Angaben zu einer Anfrage nach, für die noch kein Angebot verschickt wurde (Adressen, Etagen, Gegenstandsliste, Fotos, Wunschtermin)
- frage: Frage zu einem bestehenden Auftrag/Angebot oder zur Firma
- annahme: nimmt das Angebot an / möchte den Auftrag erteilen
- ablehnung: lehnt das Angebot ab / hat sich anders entschieden
- terminaenderung: möchte einen anderen Termin oder eine andere Uhrzeit
- stornierung: sagt einen gebuchten Auftrag ab
- zahlung: zu Rechnung, Zahlung, Überweisung, Mahnung
- beschwerde: ist unzufrieden, meldet einen Schaden
- dank: bedankt sich nur oder bestätigt etwas, ohne Frage oder Anliegen
- automatisch: Abwesenheitsnotiz, Unzustellbarkeitsmeldung, Newsletter, Werbung
- sonstiges: nichts davon

Wichtig: Wer schon ein verschicktes oder angenommenes Angebot hat, ist KEINE neue_anfrage, auch wenn er Details zum Umzug nennt — dann ist es frage, annahme, terminaenderung o.ä.
Enthält eine Mail mehrere Anliegen, nimm das wichtigste; Fragen gehen vor Dank.

Antworte NUR mit JSON, ohne Erklärung:
{"kategorie": "...", "antwort_noetig": true|false, "zusammenfassung": "ein deutscher Satz, worum es geht"}"#;

/// Build the user message for the classifier.
pub(crate) fn user_prompt(subject: &str, body: &str, facts: &str) -> String {
    format!(
        "Was wir über den Absender wissen:\n{facts}\n\n=== Neue E-Mail ===\nBetreff: {subject}\n\n{body}"
    )
}

#[derive(Deserialize)]
struct Raw {
    kategorie: String,
    #[serde(default = "yes")]
    antwort_noetig: bool,
    #[serde(default)]
    zusammenfassung: String,
}

fn yes() -> bool {
    true
}

/// Parse the classifier's answer. Tolerates prose or a code fence around the JSON.
pub(crate) fn parse(response: &str) -> Option<Classification> {
    let start = response.find('{')?;
    let end = response.rfind('}')?;
    let raw: Raw = serde_json::from_str(response.get(start..=end)?).ok()?;
    let intent = MailIntent::from_key(raw.kategorie.trim())?;
    let needs_reply = match intent {
        MailIntent::Automated | MailIntent::ThanksOnly => false,
        _ => raw.antwort_noetig,
    };
    Some(Classification { intent, needs_reply, summary: raw.zusammenfassung.trim().to_string() })
}

/// The verdict when the classifier fails: a sender with a KVA or job is answered
/// from context, anyone else goes through intake as before.
pub(crate) fn fallback(has_committed_job: bool) -> Classification {
    Classification {
        intent: if has_committed_job { MailIntent::Other } else { MailIntent::NewInquiry },
        needs_reply: true,
        summary: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_fenced_json() {
        let c = parse(r#"{"kategorie":"frage","antwort_noetig":true,"zusammenfassung":"Fragt nach dem Lager."}"#).unwrap();
        assert_eq!(c.intent, MailIntent::Question);
        assert!(c.needs_reply);
        assert_eq!(c.summary, "Fragt nach dem Lager.");

        let c = parse("```json\n{\"kategorie\": \"annahme\", \"antwort_noetig\": false, \"zusammenfassung\": \"x\"}\n```").unwrap();
        assert_eq!(c.intent, MailIntent::AcceptOffer);
        assert!(!c.needs_reply);
    }

    #[test]
    fn thanks_and_automated_never_need_a_reply() {
        let c = parse(r#"{"kategorie":"dank","antwort_noetig":true}"#).unwrap();
        assert!(!c.needs_reply);
    }

    #[test]
    fn rejects_unknown_category_and_garbage() {
        assert!(parse(r#"{"kategorie":"weiss nicht"}"#).is_none());
        assert!(parse("keine Ahnung").is_none());
    }

    #[test]
    fn fallback_depends_on_committed_job() {
        assert_eq!(fallback(true).intent, MailIntent::Other);
        assert_eq!(fallback(false).intent, MailIntent::NewInquiry);
    }

    #[test]
    fn detects_bounces_and_autoreplies() {
        assert!(is_automated("MAILER-DAEMON@mx.de", "Undelivered Mail Returned to Sender", ""));
        assert!(is_automated("anna@x.de", "Automatische Antwort: Ihr Umzugsangebot", ""));
        assert!(is_automated("anna@x.de", "Re: Angebot", "Abwesenheitsnotiz: Ich bin bis 12.10. nicht im Büro"));
        assert!(!is_automated("anna@x.de", "Re: Ihr Umzugsangebot", "Passt, danke!"));
    }
}
