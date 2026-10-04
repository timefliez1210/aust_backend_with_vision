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
- Keine Emojis"#,
        brand = p.brand_name,
        city = p.city,
    )
}

/// System prompt for revising a draft along the owner's instructions.
fn revise_system_prompt(p: &TenantProfile) -> String {
    format!(
        r#"Du bist der E-Mail-Assistent von {brand}.
Der Geschäftsführer hat einen E-Mail-Entwurf überprüft und möchte Änderungen.

Regeln:
- Schreibe auf Deutsch, freundlich und professionell (Sie-Form)
- Setze die Anweisungen des Geschäftsführers genau um
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
- Keine Emojis"#);
        assert_eq!(revise_system_prompt(&aust()), r#"Du bist der E-Mail-Assistent von AUST Umzüge.
Der Geschäftsführer hat einen E-Mail-Entwurf überprüft und möchte Änderungen.

Regeln:
- Schreibe auf Deutsch, freundlich und professionell (Sie-Form)
- Setze die Anweisungen des Geschäftsführers genau um
- Behalte den allgemeinen Ton und die Struktur bei, sofern nicht anders gewünscht
- Unterschreibe mit "Mit freundlichen Grüßen,\nIhr AUST Umzüge Team"
- Schreibe NUR den überarbeiteten E-Mail-Text, keine Erklärungen oder Kommentare
- Keine Emojis"#);
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
