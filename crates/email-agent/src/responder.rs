use crate::calendar::AvailabilityResult;
use crate::EmailError;
use aust_core::models::{InquirySource, MissingField, MovingInquiry};
use aust_core::tenant::TenantProfile;
use aust_assistant::llm::{AssistantLlmProvider, ModelTier};
use aust_llm_providers::LlmMessage;
use std::sync::Arc;
use tracing::{debug, info};

/// System prompt for the follow-up mail that asks a customer for missing details.
fn followup_system_prompt(p: &TenantProfile) -> String {
    format!(
        r#"Du bist der freundliche E-Mail-Assistent von {brand}, einem Umzugsunternehmen in {city}.
Deine Aufgabe ist es, fehlende Informationen für ein Umzugsangebot höflich und professionell einzuholen.

Regeln:
- Schreibe auf Deutsch, freundlich und professionell (Sie-Form)
- Bedanke dich für die Anfrage (nur bei der ersten E-Mail)
- Frage gezielt nach den fehlenden Informationen
- Erwähne kurz, welche Daten wir bereits haben (damit der Kunde sieht, dass wir aufmerksam sind)
- Halte die E-Mail kurz und übersichtlich
- Nummeriere die fehlenden Informationen, damit der Kunde einfach antworten kann
- Unterschreibe mit "Mit freundlichen Grüßen,\nIhr {brand} Team"
- Schreibe NUR den E-Mail-Text, keine Betreffzeile
- Erwähne, dass Fotos der Räumlichkeiten als Alternative zur Gegenstandsliste akzeptiert werden (nur wenn Volume fehlt)
- Wenn ein Terminhinweis gegeben wird (Wunschtermin nicht verfügbar), informiere den Kunden darüber und schlage die Alternativen vor
- Beantworte keine Fragen mit erfundenen Fakten (Adressen, Preise, Termine, Fristen) — schreibe stattdessen, dass wir uns dazu melden
- Keine Emojis"#,
        brand = p.brand_name,
        city = p.city,
    )
}

/// System prompt for a reply to someone we already know (or a question that is
/// not an intake). The reply may only state facts from the context block: the
/// old prompt had none and invented warehouse addresses and start dates.
fn contextual_system_prompt(p: &TenantProfile) -> String {
    format!(
        r#"Du schreibst Antwort-Entwürfe für {brand} ({company}), ein Umzugs- und Entrümpelungsunternehmen in {city}. Inhaber: {owner}. Telefon: {phone}.
Der Inhaber prüft jeden Entwurf, bevor er verschickt wird.

Du bekommst: die neue E-Mail des Kunden, die Einordnung, und ALLES, was wir über den Kunden wissen (Aufträge, Angebote, Rechnungen, bisheriger Verlauf).

Regeln:
- Beantworte genau das, was der Kunde wissen will oder mitteilt. Keine Fragen nach Umzugsdaten, die für sein Anliegen keine Rolle spielen.
- Nenne NUR Fakten, die im Kontext stehen (Termine, Uhrzeiten, Preise, Angebotsnummern, Adressen). Erfinde NIEMALS Adressen, Termine, Preise, Fristen, Verfügbarkeiten oder Zusagen.
- Was du nicht aus dem Kontext beantworten kannst: schreibe, dass wir uns dazu kurzfristig melden (oder dass der Kunde uns unter {phone} erreicht), und führe den Punkt unten unter OFFENE PUNKTE auf.
- Hat der Kunde schon einen Auftrag/Termin, beziehe dich darauf (z. B. "für Ihren Termin am 14.10.").
- Annahme eines Angebots: bedanke dich. Steht im Kontext "Termin (fest)", darfst du ihn bestätigen. Steht dort nur "Wunschtermin", schreibe NICHT "wir bestätigen den Termin", sondern z. B. "Wir haben den 14.10. für Sie vorgesehen und bestätigen Ihnen den Termin in Kürze verbindlich." und nimm "Termin verbindlich bestätigen" in die OFFENEN PUNKTE auf.
- Ablehnung: kurz und freundlich bedanken, nicht nachhaken.
- Terminänderung oder Stornierung: nichts zusagen, nur bestätigen, dass die Nachricht angekommen ist und wir uns melden.
- Deutsch, Sie-Form, freundlich, kurz. Absätze durch eine Leerzeile trennen. Keine Emojis, keine Betreffzeile, keine Platzhalter wie [Name], kein Markdown (keine Sternchen).
- Anrede mit dem Namen aus dem Kontext, falls bekannt.
- Unterschreibe mit "Mit freundlichen Grüßen\n{owner}\n{brand}"

Ausgabeformat — genau so:
<der E-Mail-Text>
=== OFFENE PUNKTE ===
- <Punkt, den der Inhaber klären muss>
(oder "keine", wenn nichts offen ist)"#,
        brand = p.brand_name,
        company = p.name,
        city = p.city,
        owner = p.owner_name,
        phone = p.phone,
    )
}

/// Marker between the reply text and the open points in the contextual reply.
const OPEN_POINTS_MARKER: &str = "=== OFFENE PUNKTE ===";

/// System prompt for revising a draft along the owner's instructions.
fn revise_system_prompt(p: &TenantProfile) -> String {
    format!(
        r#"Du bist der E-Mail-Assistent von {brand}.
Der Geschäftsführer hat einen E-Mail-Entwurf überprüft und möchte Änderungen.

Regeln:
- Schreibe auf Deutsch, freundlich und professionell (Sie-Form)
- Setze die Anweisungen des Geschäftsführers genau um
- Erfinde keine Fakten (Adressen, Termine, Preise, Fristen), die weder im Entwurf noch in der Anweisung stehen
- Behalte den allgemeinen Ton und die Struktur bei, sofern nicht anders gewünscht
- Unterschreibe mit "Mit freundlichen Grüßen,\nIhr {brand} Team"
- Schreibe NUR den überarbeiteten E-Mail-Text, keine Erklärungen oder Kommentare
- Keine Emojis"#,
        brand = p.brand_name,
    )
}

pub struct EmailResponder {
    /// Email replies are generated through the assistant's LLM (Josie's model
    /// path) rather than the generic provider. That path issues every `/api/chat`
    /// request through a retrying client with a generous timeout, which avoids the
    /// "Network error: error sending request" failures the old non-retrying,
    /// 60 s `LlmProvider::complete()` path produced on Ollama Cloud.
    llm: Arc<dyn AssistantLlmProvider>,
    /// The company the mailbox belongs to — names and phone in every reply.
    profile: TenantProfile,
}

impl EmailResponder {
    pub fn new(llm: Arc<dyn AssistantLlmProvider>, profile: TenantProfile) -> Self {
        Self { llm, profile }
    }

    /// Generate a response email for a new or ongoing inquiry.
    /// If the inquiry is complete, returns a confirmation.
    /// If data is missing, generates a friendly German email asking for it.
    /// If availability info is provided and the date is unavailable, the LLM is instructed
    /// to inform the customer and suggest alternative dates.
    pub async fn generate_response(
        &self,
        inquiry: &MovingInquiry,
        original_body: &str,
        availability: Option<&AvailabilityResult>,
    ) -> Result<EmailResponse, EmailError> {
        let missing = inquiry.missing_fields();

        if inquiry.is_complete() {
            info!("Inquiry {} is complete, generating confirmation", inquiry.id);
            return Ok(self.generate_confirmation(inquiry));
        }

        info!(
            "Inquiry {} missing {} fields, generating follow-up",
            inquiry.id,
            missing.len()
        );

        let response_body = self
            .generate_followup_with_llm(inquiry, &missing, original_body, availability)
            .await?;

        let subject = match inquiry.source {
            InquirySource::QuoteForm => {
                format!("Re: Ihr kostenloses Angebot bei {}", self.profile.brand_name)
            }
            _ => format!("Re: Ihre Anfrage bei {}", self.profile.brand_name),
        };

        Ok(EmailResponse {
            subject,
            body: response_body,
        })
    }

    /// Decide what an incoming mail wants. `None` when the LLM fails or answers
    /// garbage; the caller then falls back on [`crate::intent::fallback`].
    pub(crate) async fn classify(
        &self,
        subject: &str,
        body: &str,
        facts: &str,
    ) -> Option<crate::intent::Classification> {
        let messages = vec![
            LlmMessage::system(crate::intent::SYSTEM_PROMPT.to_string()),
            LlmMessage::user(crate::intent::user_prompt(subject, body, facts)),
        ];
        match self.llm.chat(ModelTier::Main, &messages).await {
            Ok(r) => {
                let parsed = crate::intent::parse(&r);
                if parsed.is_none() {
                    tracing::warn!("Intent classifier returned unparseable output");
                }
                parsed
            }
            Err(e) => {
                tracing::warn!("Intent classification failed: {e}");
                None
            }
        }
    }

    /// Draft a reply grounded in what we know about the customer. Returns the
    /// mail and the points Alex has to settle himself (never sent to the customer).
    pub(crate) async fn generate_contextual_reply(
        &self,
        subject: &str,
        body: &str,
        classification: &crate::intent::Classification,
        facts: &str,
    ) -> Result<ContextualReply, EmailError> {
        let user_prompt = format!(
            "Was wir über den Kunden wissen:\n{facts}\n\n\
             Einordnung der neuen E-Mail: {label}{summary}\n\n\
             === Neue E-Mail des Kunden ===\nBetreff: {subject}\n\n{body}\n\n\
             Schreibe den Antwort-Entwurf.",
            label = classification.intent.label(),
            summary = if classification.summary.is_empty() {
                String::new()
            } else {
                format!(" — {}", classification.summary)
            },
        );
        let messages = vec![
            LlmMessage::system(contextual_system_prompt(&self.profile)),
            LlmMessage::user(user_prompt),
        ];
        let response = self
            .llm
            .chat(ModelTier::Main, &messages)
            .await
            .map_err(|e| EmailError::Llm(e.to_string()))?;
        Ok(split_open_points(&response))
    }

    /// Use the LLM to generate a natural, friendly German follow-up email
    /// that requests the missing information.
    async fn generate_followup_with_llm(
        &self,
        inquiry: &MovingInquiry,
        missing: &[MissingField],
        original_body: &str,
        availability: Option<&AvailabilityResult>,
    ) -> Result<String, EmailError> {
        let known_data = format_known_data(inquiry);
        let missing_list = missing
            .iter()
            .map(|f| format!("- {}", f.german_prompt()))
            .collect::<Vec<_>>()
            .join("\n");

        let availability_context = if let Some(avail) = availability {
            if !avail.requested_date_available {
                let alternatives: Vec<String> = avail
                    .alternatives
                    .iter()
                    .map(|a| a.date.format("%d.%m.%Y").to_string())
                    .collect();
                let alt_text = if alternatives.is_empty() {
                    "Es sind leider keine Alternativtermine in den nächsten 14 Tagen verfügbar.".to_string()
                } else {
                    format!(
                        "Alternativtermine: {}",
                        alternatives.join(", ")
                    )
                };
                format!(
                    "\n\nWICHTIG - TERMINVERFÜGBARKEIT:\n\
                     Der Wunschtermin {} ist leider bereits ausgebucht.\n\
                     {}\n\
                     Informiere den Kunden höflich, dass der Wunschtermin nicht verfügbar ist, \
                     und schlage die Alternativtermine vor. Frage, ob einer der Alternativen passt.",
                    avail.requested_date.format("%d.%m.%Y"),
                    alt_text
                )
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let system_prompt = followup_system_prompt(&self.profile);

        let user_prompt = format!(
            "Der Kunde hat folgende Anfrage geschickt:\n\n---\n{original_body}\n---\n\n\
             Bereits bekannte Daten:\n{known_data}\n\n\
             Fehlende Informationen:\n{missing_list}{availability_context}\n\n\
             Generiere eine Antwort-E-Mail, die die fehlenden Informationen anfragt."
        );

        debug!("Generating follow-up email via LLM");

        let messages = vec![
            LlmMessage::system(system_prompt.to_string()),
            LlmMessage::user(user_prompt),
        ];

        let response = self
            .llm
            .chat(ModelTier::Main, &messages)
            .await
            .map_err(|e| EmailError::Llm(e.to_string()))?;

        Ok(response)
    }

    /// Generate a confirmation email when all data is collected.
    fn generate_confirmation(&self, inquiry: &MovingInquiry) -> EmailResponse {
        confirmation(&self.profile, inquiry)
    }

    /// Revise a draft email based on the admin's instructions.
    /// This is called when Alex presses "Bearbeiten" and sends feedback
    /// like "Mach es kürzer" or "Frag auch nach dem Aufzug".
    /// Returns a new EmailResponse with the revised draft.
    pub async fn revise_draft(
        &self,
        original_draft: &str,
        admin_instructions: &str,
        subject: &str,
    ) -> Result<EmailResponse, EmailError> {
        let system_prompt = revise_system_prompt(&self.profile);

        let user_prompt = format!(
            "Hier ist der aktuelle Entwurf:\n\n---\n{original_draft}\n---\n\n\
             Anweisung vom Geschäftsführer:\n{admin_instructions}\n\n\
             Bitte überarbeite den Entwurf entsprechend."
        );

        debug!("Revising draft via LLM: {}", crate::text::truncate_on_char_boundary(admin_instructions, 80));

        let messages = vec![
            LlmMessage::system(system_prompt.to_string()),
            LlmMessage::user(user_prompt),
        ];

        let response = self
            .llm
            .chat(ModelTier::Main, &messages)
            .await
            .map_err(|e| EmailError::Llm(e.to_string()))?;

        Ok(EmailResponse {
            subject: subject.to_string(),
            body: response,
        })
    }

    /// Use LLM to extract structured data from a free-text email.
    /// Returns an updated MovingInquiry with any additional fields found.
    pub async fn extract_data_from_text(
        &self,
        inquiry: &MovingInquiry,
        email_body: &str,
    ) -> Result<MovingInquiry, EmailError> {
        let system_prompt = r#"Du bist ein Daten-Extrahierer. Analysiere die folgende E-Mail eines Umzugskunden und extrahiere alle relevanten Informationen im JSON-Format.

Extrahiere diese Felder (wenn vorhanden):
{
  "name": "vollständiger Name",
  "phone": "Telefonnummer",
  "scheduled_date": "YYYY-MM-DD",
  "departure_address": "vollständige Auszugsadresse",
  "departure_floor": "Stockwerk (z.B. Erdgeschoss, 2. Stock)",
  "arrival_address": "vollständige Einzugsadresse",
  "arrival_floor": "Stockwerk",
  "volume_m3": Zahl oder null,
  "items_description": "Beschreibung der Gegenstände",
  "notes": "sonstige relevante Informationen"
}

Antworte NUR mit dem JSON-Objekt, ohne Erklärungen. Setze fehlende Felder auf null."#;

        let messages = vec![
            LlmMessage::system(system_prompt.to_string()),
            LlmMessage::user(email_body.to_string()),
        ];

        let response = self
            .llm
            .chat(ModelTier::Main, &messages)
            .await
            .map_err(|e| EmailError::Llm(e.to_string()))?;

        // Try to parse the LLM's JSON response and merge into inquiry
        let mut updated = inquiry.clone();

        if let Ok(extracted) = serde_json::from_str::<serde_json::Value>(&response) {
            if let Some(name) = extracted.get("name").and_then(|v| v.as_str())
                && updated.name.is_none() && !name.is_empty() {
                    updated.name = Some(name.to_string());
                }
            if let Some(phone) = extracted.get("phone").and_then(|v| v.as_str())
                && updated.phone.is_none() && !phone.is_empty() {
                    updated.phone = Some(phone.to_string());
                }
            if let Some(date_str) = extracted.get("scheduled_date").and_then(|v| v.as_str())
                && updated.scheduled_date.is_none()
                    && let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                        updated.scheduled_date = Some(date);
                    }
            if let Some(addr) = extracted.get("departure_address").and_then(|v| v.as_str())
                && updated.departure_address.is_none() && !addr.is_empty() {
                    updated.departure_address = Some(addr.to_string());
                }
            if let Some(floor) = extracted.get("departure_floor").and_then(|v| v.as_str())
                && updated.departure_floor.is_none() && !floor.is_empty() {
                    updated.departure_floor = Some(floor.to_string());
                }
            if let Some(addr) = extracted.get("arrival_address").and_then(|v| v.as_str())
                && updated.arrival_address.is_none() && !addr.is_empty() {
                    updated.arrival_address = Some(addr.to_string());
                }
            if let Some(floor) = extracted.get("arrival_floor").and_then(|v| v.as_str())
                && updated.arrival_floor.is_none() && !floor.is_empty() {
                    updated.arrival_floor = Some(floor.to_string());
                }
            if let Some(vol) = extracted.get("volume_m3").and_then(|v| v.as_f64())
                && updated.volume_m3.is_none() {
                    updated.volume_m3 = Some(vol);
                }
            if let Some(items) = extracted.get("items_description").and_then(|v| v.as_str())
                && updated.items_list.is_none() && !items.is_empty() {
                    updated.items_list = Some(items.to_string());
                }
            if let Some(notes) = extracted.get("notes").and_then(|v| v.as_str())
                && !notes.is_empty() {
                    let existing = updated.notes.clone().unwrap_or_default();
                    if !existing.contains(notes) {
                        updated.notes = Some(if existing.is_empty() {
                            notes.to_string()
                        } else {
                            format!("{existing}\n{notes}")
                        });
                    }
                }
        }

        Ok(updated)
    }
}

/// Format known data into a readable summary for the LLM prompt.
fn format_known_data(inquiry: &MovingInquiry) -> String {
    let mut lines = Vec::new();

    if let Some(name) = &inquiry.name {
        lines.push(format!("- Name: {name}"));
    }
    lines.push(format!("- E-Mail: {}", inquiry.email));
    if let Some(phone) = &inquiry.phone {
        lines.push(format!("- Telefon: {phone}"));
    }
    if let Some(date) = inquiry.scheduled_date {
        lines.push(format!("- Wunschtermin: {}", date.format("%d.%m.%Y")));
    }
    if let Some(addr) = &inquiry.departure_address {
        lines.push(format!("- Auszugsadresse: {addr}"));
    }
    if let Some(floor) = &inquiry.departure_floor {
        lines.push(format!("- Etage Auszug: {floor}"));
    }
    if let Some(addr) = &inquiry.arrival_address {
        lines.push(format!("- Einzugsadresse: {addr}"));
    }
    if let Some(floor) = &inquiry.arrival_floor {
        lines.push(format!("- Etage Einzug: {floor}"));
    }
    if let Some(vol) = inquiry.volume_m3 {
        lines.push(format!("- Volumen: {vol:.1} m³"));
    }
    if inquiry.items_list.is_some() {
        lines.push("- Gegenstandsliste: vorhanden".to_string());
    }
    if inquiry.has_photos {
        lines.push(format!("- Fotos: {} Stück", inquiry.photo_count));
    }

    if lines.is_empty() {
        "Noch keine Daten vorhanden.".to_string()
    } else {
        lines.join("\n")
    }
}

/// Format the selected additional services.
/// Confirmation mail once every detail of an inquiry is in.
fn confirmation(p: &TenantProfile, inquiry: &MovingInquiry) -> EmailResponse {
    let name = inquiry.name.as_deref().unwrap_or("Kunde");
    let services = format_services(inquiry);

    let body = format!(
        "Sehr geehrte/r {name},\n\n\
         vielen Dank für Ihre vollständigen Angaben! Wir haben alle Informationen \
         erhalten und erstellen nun Ihr individuelles Angebot.\n\n\
         Zusammenfassung Ihrer Anfrage:\n\
         - Auszugsadresse: {departure}\n\
         - Einzugsadresse: {arrival}\n\
         - Wunschtermin: {date}\n\
         - Geschätztes Volumen: {volume}\n\
         {services}\
         \n\
         Sie erhalten Ihr kostenloses Angebot in Kürze per E-Mail.\n\n\
         Bei Rückfragen erreichen Sie uns jederzeit unter {phone}.\n\n\
         Mit freundlichen Grüßen,\n\
         Ihr {brand} Team",
        departure = inquiry.departure_address.as_deref().unwrap_or("-"),
        arrival = inquiry.arrival_address.as_deref().unwrap_or("-"),
        date = inquiry
            .scheduled_date
            .map(|d| d.format("%d.%m.%Y").to_string())
            .unwrap_or_else(|| "-".to_string()),
        volume = inquiry
            .volume_m3
            .map(|v| format!("{v:.1} m³"))
            .or_else(|| inquiry.items_list.as_ref().map(|_| "gemäß Gegenstandsliste".to_string()))
            .or_else(|| {
                if inquiry.has_photos {
                    Some("wird anhand Ihrer Fotos geschätzt".to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "-".to_string()),
        phone = p.phone,
        brand = p.brand_name,
    );

    EmailResponse {
        subject: format!("Ihr Umzugsangebot wird erstellt – {}", p.brand_name),
        body,
    }
}

fn format_services(inquiry: &MovingInquiry) -> String {
    let mut services = Vec::new();
    if inquiry.service_packing {
        services.push("Einpackservice");
    }
    if inquiry.service_assembly {
        services.push("Möbelmontage");
    }
    if inquiry.service_disassembly {
        services.push("Möbeldemontage");
    }
    if inquiry.service_storage {
        services.push("Einlagerung");
    }
    if inquiry.service_disposal {
        services.push("Entsorgung");
    }

    if services.is_empty() {
        String::new()
    } else {
        format!("- Zusatzleistungen: {}\n", services.join(", "))
    }
}

/// A contextual reply draft plus what only Alex can answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContextualReply {
    pub body: String,
    pub open_points: Vec<String>,
}

/// Split the LLM output at [`OPEN_POINTS_MARKER`]. Without the marker the whole
/// text is the mail.
fn split_open_points(response: &str) -> ContextualReply {
    let (body, rest) = match response.find(OPEN_POINTS_MARKER) {
        Some(i) => (&response[..i], &response[i + OPEN_POINTS_MARKER.len()..]),
        None => (response, ""),
    };
    let open_points = rest
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
        .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("keine") && !l.eq_ignore_ascii_case("keine."))
        .map(str::to_string)
        .collect();
    ContextualReply { body: body.trim().to_string(), open_points }
}

#[derive(Debug, Clone)]
pub struct EmailResponse {
    pub subject: String,
    pub body: String,
}

use chrono::NaiveDate;

#[cfg(test)]
mod tests {
    use super::*;

    fn aust() -> TenantProfile {
        TenantProfile {
            id: aust_core::tenant::AUST,
            name: "Aust Umzüge & Haushaltsauflösungen".into(),
            short_name: "Aust Umzüge".into(),
            brand_name: "AUST Umzüge".into(),
            owner_name: "Alex Aust".into(),
            phone: "05121 – 7558379".into(),
            city: "Hildesheim".into(),
            review_url: String::new(),
            depot_address: String::new(),
        }
    }

    /// Golden: Aust's prompts are character for character what they were before the
    /// brand and town moved into the tenant profile.
    #[test]
    fn aust_prompts_are_unchanged() {
        assert_eq!(followup_system_prompt(&aust()), r#"Du bist der freundliche E-Mail-Assistent von AUST Umzüge, einem Umzugsunternehmen in Hildesheim.
Deine Aufgabe ist es, fehlende Informationen für ein Umzugsangebot höflich und professionell einzuholen.

Regeln:
- Schreibe auf Deutsch, freundlich und professionell (Sie-Form)
- Bedanke dich für die Anfrage (nur bei der ersten E-Mail)
- Frage gezielt nach den fehlenden Informationen
- Erwähne kurz, welche Daten wir bereits haben (damit der Kunde sieht, dass wir aufmerksam sind)
- Halte die E-Mail kurz und übersichtlich
- Nummeriere die fehlenden Informationen, damit der Kunde einfach antworten kann
- Unterschreibe mit "Mit freundlichen Grüßen,\nIhr AUST Umzüge Team"
- Schreibe NUR den E-Mail-Text, keine Betreffzeile
- Erwähne, dass Fotos der Räumlichkeiten als Alternative zur Gegenstandsliste akzeptiert werden (nur wenn Volume fehlt)
- Wenn ein Terminhinweis gegeben wird (Wunschtermin nicht verfügbar), informiere den Kunden darüber und schlage die Alternativen vor
- Beantworte keine Fragen mit erfundenen Fakten (Adressen, Preise, Termine, Fristen) — schreibe stattdessen, dass wir uns dazu melden
- Keine Emojis"#);
        assert_eq!(revise_system_prompt(&aust()), r#"Du bist der E-Mail-Assistent von AUST Umzüge.
Der Geschäftsführer hat einen E-Mail-Entwurf überprüft und möchte Änderungen.

Regeln:
- Schreibe auf Deutsch, freundlich und professionell (Sie-Form)
- Setze die Anweisungen des Geschäftsführers genau um
- Erfinde keine Fakten (Adressen, Termine, Preise, Fristen), die weder im Entwurf noch in der Anweisung stehen
- Behalte den allgemeinen Ton und die Struktur bei, sofern nicht anders gewünscht
- Unterschreibe mit "Mit freundlichen Grüßen,\nIhr AUST Umzüge Team"
- Schreibe NUR den überarbeiteten E-Mail-Text, keine Erklärungen oder Kommentare
- Keine Emojis"#);
    }

    #[test]
    fn open_points_are_split_off() {
        let r = split_open_points(
            "Sehr geehrte Frau Sharma,\n\nwir melden uns.\n\nMit freundlichen Grüßen\n=== OFFENE PUNKTE ===\n- Lageradresse nennen\n* Starttermin klären\n",
        );
        assert_eq!(r.body, "Sehr geehrte Frau Sharma,\n\nwir melden uns.\n\nMit freundlichen Grüßen");
        assert_eq!(r.open_points, vec!["Lageradresse nennen", "Starttermin klären"]);

        let r = split_open_points("Danke!\n=== OFFENE PUNKTE ===\nkeine");
        assert!(r.open_points.is_empty());
        let r = split_open_points("Nur Text");
        assert_eq!(r.body, "Nur Text");
    }

    #[test]
    fn contextual_prompt_forbids_invention() {
        let p = contextual_system_prompt(&aust());
        assert!(p.contains("Erfinde NIEMALS"));
        assert!(p.contains("05121 – 7558379"));
        assert!(p.contains(OPEN_POINTS_MARKER));
    }

    /// Golden: Aust's confirmation mail is unchanged.
    #[test]
    fn aust_confirmation_is_unchanged() {
        let inquiry = MovingInquiry {
            name: Some("Frau Schilling".into()),
            departure_address: Some("Steinbergstr. 3, 31139 Hildesheim".into()),
            arrival_address: Some("Kaiserstr. 32, 31134 Hildesheim".into()),
            scheduled_date: chrono::NaiveDate::from_ymd_opt(2026, 11, 2),
            volume_m3: Some(24.0),
            service_packing: true,
            ..Default::default()
        };
        let mail = confirmation(&aust(), &inquiry);
        assert_eq!(mail.subject, "Ihr Umzugsangebot wird erstellt – AUST Umzüge");
        assert_eq!(
            mail.body,
            "Sehr geehrte/r Frau Schilling,\n\nvielen Dank für Ihre vollständigen Angaben! Wir haben \
             alle Informationen erhalten und erstellen nun Ihr individuelles Angebot.\n\n\
             Zusammenfassung Ihrer Anfrage:\n\
             - Auszugsadresse: Steinbergstr. 3, 31139 Hildesheim\n\
             - Einzugsadresse: Kaiserstr. 32, 31134 Hildesheim\n\
             - Wunschtermin: 02.11.2026\n\
             - Geschätztes Volumen: 24.0 m³\n\
             - Zusatzleistungen: Einpackservice\n\
             \n\
             Sie erhalten Ihr kostenloses Angebot in Kürze per E-Mail.\n\n\
             Bei Rückfragen erreichen Sie uns jederzeit unter 05121 – 7558379.\n\n\
             Mit freundlichen Grüßen,\nIhr AUST Umzüge Team"
        );
    }
}

/// Live check of the classifier and the grounded reply against real mails.
/// `set -a; . ./.env; cargo test -p aust-email-agent live_replies -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::*;
    use crate::context::{JobFacts, MailContext};
    use chrono::{DateTime, NaiveDate, NaiveTime, Utc};

    fn llm() -> Arc<dyn AssistantLlmProvider> {
        let base = std::env::var("AUST__LLM__OLLAMA__BASE_URL").unwrap_or("https://ollama.com".into());
        let key = std::env::var("AUST__LLM__OLLAMA__API_KEY").ok();
        let model = std::env::var("AUST__LLM__OLLAMA__ASSISTANT_MODEL").unwrap_or("gpt-oss:120b".into());
        Arc::new(aust_assistant::llm::OllamaAssistantLlm::new(base, key).with_models(model.clone(), model))
    }

    fn sharma() -> MailContext {
        MailContext {
            customer_name: Some("Vera Sharma".into()),
            customer_phone: None,
            jobs: vec![JobFacts {
                status: "accepted".into(),
                service_type: Some("entruempelung".into()),
                scheduled_date: NaiveDate::from_ymd_opt(2026, 10, 14),
                end_date: NaiveDate::from_ymd_opt(2026, 10, 15),
                start_time: NaiveTime::from_hms_opt(8, 0, 0),
                origin: Some("Knollenstr. 5, 31134 Hildesheim".into()),
                destination: None,
                volume_m3: Some(165.0),
                offer_number: Some("2026-0359".into()),
                offer_brutto_cents: Some(615_000),
                offer_sent_at: Some(DateTime::parse_from_rfc3339("2026-10-05T20:52:25Z").unwrap().with_timezone(&Utc)),
                created_at: DateTime::parse_from_rfc3339("2026-10-05T19:24:45Z").unwrap().with_timezone(&Utc),
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    #[ignore]
    async fn live_replies() {
        let r = EmailResponder::new(llm(), tests_profile());
        let today = NaiveDate::from_ymd_opt(2026, 10, 6).unwrap();
        let cases: Vec<(&str, &str, &str, MailContext)> = vec![
            ("Sharma Fragen", "Re: Ihr Umzugsangebot",
             "Hallo Herr Aust,\n\nDanke für das Angebot. Auf dieser Grundlage möchten wir die Entrümpelung und Einlagerung mit Ihnen machen.\n\nIch habe noch ein paar Fragen:\nKönnen Sie die Bohrlöcher auch verdecken?\nWo ist Ihr Lager? Könnte man die 6 Monate noch später verlängern falls nötig? Wie sind da die Vorlauffristen?\nNehmen Sie den Bauschutt auch mit und entsorgen ihn? (Auf dem Dachboden über der Garage)\nAb wann können Sie starten und muss die ganze Zeit jemand anwesend sein?\n\nGerne können Sie mich anrufen wenn es passt und wir besprechen die Details.\n\nViele Grüße\nVera Sharma",
             sharma()),
            ("Annahme", "AW: Ihr Umzugsangebot",
             "Guten Tag,\n\nwir nehmen Ihr Angebot gerne an. Bitte bestätigen Sie uns den Termin.\n\nMfG\nThomas Krüger",
             MailContext { customer_name: Some("Thomas Krüger".into()), jobs: vec![JobFacts { status: "offer_sent".into(), service_type: Some("umzug".into()), end_date: None, origin: Some("Almsstr. 3, 31134 Hildesheim".into()), destination: Some("Podbielskistr. 10, 30177 Hannover".into()), volume_m3: Some(28.0), offer_number: Some("2026-0340".into()), offer_brutto_cents: Some(189_000), ..sharma().jobs[0].clone() }], ..Default::default() }),
            ("Neue Anfrage", "Umzug Dezember",
             "Hallo, wir ziehen im Dezember mit einer 3-Zimmer-Wohnung von Hildesheim nach Hannover. Was würde das ungefähr kosten?\nGruß Lena Bauer",
             MailContext::default()),
            ("Dank", "Re: Rechnung 2026-37",
             "Vielen Dank, ist überwiesen. Die Jungs waren super!\nViele Grüße",
             MailContext { customer_name: Some("Petra Lange".into()), jobs: vec![JobFacts { status: "paid".into(), ..sharma().jobs[0].clone() }], ..Default::default() }),
        ];
        for (name, subject, body, ctx) in cases {
            let facts = ctx.facts(today);
            let c = r.classify(subject, body, &facts).await;
            println!("\n================ {name} ================\n{c:?}");
            let Some(c) = c else { continue };
            if c.needs_reply && !(c.intent.is_intake() && !ctx.has_committed_job()) {
                let reply = r.generate_contextual_reply(subject, body, &c, &facts).await.unwrap();
                println!("--- Entwurf ---\n{}\n--- Offene Punkte ---\n{:#?}", reply.body, reply.open_points);
            }
        }
    }

    fn tests_profile() -> TenantProfile {
        TenantProfile {
            id: aust_core::tenant::AUST,
            name: "Aust Umzüge & Haushaltsauflösungen".into(),
            short_name: "Aust Umzüge".into(),
            brand_name: "AUST Umzüge".into(),
            owner_name: "Alex Aust".into(),
            phone: "05121 – 7558379".into(),
            city: "Hildesheim".into(),
            review_url: String::new(),
            depot_address: String::new(),
        }
    }
}
