//! Nightly reflection — how Josie learns from the day.
//!
//! Once a night (slot `reflection` in `agent_briefing_log`, early morning
//! Europe/Berlin) the main model reads the last 24 hours: what Josie did
//! (episodes), how Alex reacted to her proposals (confirmed / cancelled /
//! edited), and the conversation itself — where his corrections live ("nein,
//! immer erst die Besichtigung", "Frau Lange will nur per Mail"). From that it
//! proposes at most a few durable lessons.
//!
//! Nothing is stored on the model's say-so. Every lesson goes to
//! `pending_memory_proposals` and is posted to Alex with ✅ Merken / ❌ Nein
//! buttons (`mp:<id>:ok|no`); only his tap writes `agent_memory`. The model's
//! input includes customer emails, which an attacker controls, so its own
//! confidence is never authority enough to plant a rule.

use chrono::{Timelike, Utc};
use chrono_tz::Europe::Berlin;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use aust_llm_providers::LlmMessage;

use crate::error::Result;
use crate::events::TelegramNotifier;
use crate::hooks::briefing;
use crate::llm::{AssistantLlmProvider, ModelTier};
use crate::memory::durable::{self, MemoryKind};
use crate::memory::episodic;
use crate::memory::proposals::{self, NewProposal};

/// Reflection runs from this hour (Berlin) on, so the lessons are waiting in the
/// chat before the 07:00 briefing.
const REFLECTION_HOUR: u32 = 6;
/// A missed hour (restart, outage) is caught up for this long.
const CATCHUP_HOURS: u32 = 2;
/// Most lessons proposed per night: Alex reviews them by hand.
const MAX_LESSONS: usize = 3;
/// Lessons the model is less sure of are not worth Alex's tap.
const MIN_CONFIDENCE: f64 = 0.6;
/// Character budgets for the model input.
const TURNS_BUDGET: usize = 8_000;
const EPISODES_BUDGET: usize = 6_000;
const MEMORY_BUDGET: usize = 3_000;

const PROMPT: &str = include_str!("../../prompts/reflection_nightly.md");

/// One lesson as the model returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct Lesson {
    pub kind: String,
    pub scope: String,
    pub key: String,
    pub value: Value,
    pub confidence: f64,
    #[serde(default)]
    pub evidence: String,
}

/// A lesson that made it into `pending_memory_proposals`.
#[derive(Debug, Clone)]
pub struct QueuedLesson {
    pub proposal_id: Uuid,
    pub lesson: Lesson,
}

/// The 60 s tick: run the reflection once per night and post its proposals.
pub async fn run_reflection_tick(
    pool: &PgPool,
    llm: &dyn AssistantLlmProvider,
    notifier: &dyn TelegramNotifier,
) -> Result<()> {
    let now = Utc::now().with_timezone(&Berlin);
    let hour = now.hour();
    if hour < REFLECTION_HOUR || hour >= REFLECTION_HOUR + CATCHUP_HOURS {
        return Ok(());
    }
    let Some(chat) = briefing::owner_chat(pool).await? else { return Ok(()) };
    let today = now.date_naive();
    if !briefing::claim_slot(pool, today, "reflection", chat).await? {
        return Ok(());
    }

    let queued = match reflect_on_last_day(pool, llm).await {
        Ok(q) => q,
        Err(e) => {
            // Leave the night unclaimed so the next tick tries again.
            briefing::release_slot(pool, today, "reflection").await;
            return Err(e);
        }
    };
    info!(lessons = queued.len(), "Nightly reflection complete");
    for q in &queued {
        post_proposal(pool, notifier, chat, q).await;
    }
    Ok(())
}

/// Read the last 24 hours and queue up to `MAX_LESSONS` lessons for approval.
pub async fn reflect_on_last_day(pool: &PgPool, llm: &dyn AssistantLlmProvider) -> Result<Vec<QueuedLesson>> {
    let since = Utc::now() - chrono::Duration::hours(24);

    let episodes: Vec<String> = episodic::fetch_recent(pool, 120)
        .await?
        .into_iter()
        .filter(|e| e.created_at >= since)
        .map(|e| format!("- {}", e.summary))
        .collect();
    let turns = recent_turns(pool, since).await?;
    if episodes.is_empty() && turns.is_empty() {
        return Ok(Vec::new());
    }

    let memories = durable::recall(pool, None, None).await?;
    let known: Vec<String> = memories
        .iter()
        .map(|m| format!("- [{}] {} / {}: {}", m.kind, m.scope, m.key, m.value))
        .collect();
    let pending = proposals::list_pending(pool, 200).await?;

    let input = format!(
        "{PROMPT}\n\n## Schon gespeichert (nicht erneut vorschlagen)\n{}\n\n\
         ## Was Josie gestern getan hat und wie Alex reagiert hat\n{}\n\n\
         ## Gesprächsverlauf der letzten 24 Stunden\n{}",
        cap(&known.join("\n"), MEMORY_BUDGET),
        cap(&episodes.join("\n"), EPISODES_BUDGET),
        cap(&turns, TURNS_BUDGET),
    );
    let reply = llm.chat(ModelTier::Main, &[LlmMessage::user(input)]).await?;

    let mut queued = Vec::new();
    for lesson in parse_lessons(&reply) {
        if queued.len() == MAX_LESSONS {
            break;
        }
        if !is_acceptable(&lesson) {
            continue;
        }
        let duplicate = memories.iter().any(|m| m.scope == lesson.scope && m.key == lesson.key && m.value == lesson.value)
            || pending.iter().any(|p| p.scope == lesson.scope && p.key == lesson.key)
            || queued.iter().any(|q: &QueuedLesson| q.lesson.scope == lesson.scope && q.lesson.key == lesson.key);
        if duplicate {
            continue;
        }
        let rationale = format!("nightly_reflection: {}", lesson.evidence);
        match proposals::enqueue(
            pool,
            NewProposal {
                session_id: None,
                kind: parse_kind(&lesson.kind),
                scope: &lesson.scope,
                key: &lesson.key,
                value: lesson.value.clone(),
                confidence: lesson.confidence as f32,
                source_episodes: vec![],
                rationale: Some(&rationale),
            },
        )
        .await
        {
            Ok(proposal_id) => queued.push(QueuedLesson { proposal_id, lesson }),
            Err(e) => warn!(key = lesson.key, "Failed to queue lesson: {e}"),
        }
    }
    Ok(queued)
}

/// Post one queued lesson with its approve/reject buttons.
async fn post_proposal(pool: &PgPool, notifier: &dyn TelegramNotifier, chat: i64, q: &QueuedLesson) {
    let text = proposal_text(&q.lesson);
    let markup = json!({
        "inline_keyboard": [[
            { "text": "✅ Merken", "callback_data": format!("mp:{}:ok", q.proposal_id) },
            { "text": "❌ Nein",   "callback_data": format!("mp:{}:no", q.proposal_id) },
        ]]
    });
    match notifier.post_with_markup(chat, text, markup).await {
        Ok(message_id) => {
            crate::message_refs::record(pool, chat, message_id, json!({ "memory_proposal_id": q.proposal_id.to_string() }))
                .await
        }
        Err(e) => warn!(proposal = %q.proposal_id, "Failed to post lesson: {e}"),
    }
}

/// The Telegram text for a proposed lesson.
pub fn proposal_text(lesson: &Lesson) -> String {
    let value = match &lesson.value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let scope = if lesson.scope == "global" { "allgemein".to_string() } else { lesson.scope.clone() };
    let mut text = format!("🧠 Soll ich mir das merken?\n\n{value}\n\n({}, {scope})", kind_label(&lesson.kind));
    if !lesson.evidence.trim().is_empty() {
        text.push_str(&format!("\nGrund: {}", lesson.evidence.trim()));
    }
    text
}

/// The user/assistant turns of every session from `since` on, oldest first.
async fn recent_turns(pool: &PgPool, since: chrono::DateTime<Utc>) -> Result<String> {
    let rows: Vec<(Value,)> = sqlx::query_as("SELECT turns FROM agent_sessions WHERE updated_at >= $1")
        .bind(since)
        .fetch_all(pool)
        .await?;
    let mut turns: Vec<crate::session::Turn> = rows
        .into_iter()
        .filter_map(|(v,)| serde_json::from_value::<Vec<crate::session::Turn>>(v).ok())
        .flatten()
        .filter(|t| t.ts >= since)
        .collect();
    turns.sort_by_key(|t| t.ts);
    Ok(turns
        .iter()
        .map(|t| {
            let who = if t.role == "user" { "Alex" } else { "Josie" };
            let content: String = t.content.chars().take(500).collect();
            format!("{who}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The lessons in a model reply: the first JSON array in it, or none.
pub(crate) fn parse_lessons(reply: &str) -> Vec<Lesson> {
    let (Some(start), Some(end)) = (reply.find('['), reply.rfind(']')) else { return Vec::new() };
    if end <= start {
        return Vec::new();
    }
    serde_json::from_str(&reply[start..=end]).unwrap_or_default()
}

/// Shape checks before a lesson may bother Alex.
pub(crate) fn is_acceptable(l: &Lesson) -> bool {
    let kind_ok = matches!(l.kind.as_str(), "preference" | "fact" | "rule" | "pattern");
    let scope_ok = l.scope == "global"
        || l.scope.split_once(':').is_some_and(|(prefix, id)| {
            matches!(prefix, "customer" | "inquiry" | "employee") && Uuid::parse_str(id).is_ok()
        });
    let key_ok = !l.key.trim().is_empty() && l.key.chars().count() <= 80;
    let value_ok = match &l.value {
        Value::String(s) => !s.trim().is_empty(),
        Value::Null => false,
        _ => true,
    };
    kind_ok && scope_ok && key_ok && value_ok && l.confidence >= MIN_CONFIDENCE
}

fn parse_kind(s: &str) -> MemoryKind {
    match s {
        "preference" => MemoryKind::Preference,
        "rule" => MemoryKind::Rule,
        "pattern" => MemoryKind::Pattern,
        _ => MemoryKind::Fact,
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "rule" => "Regel",
        "preference" => "Vorliebe",
        "pattern" => "Muster",
        _ => "Fakt",
    }
}

/// Keep the newest end of `text` within `max` characters.
fn cap(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    text.chars().skip(n - max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockAssistantLlm;

    fn lesson(kind: &str, scope: &str, key: &str, confidence: f64) -> Lesson {
        Lesson {
            kind: kind.into(),
            scope: scope.into(),
            key: key.into(),
            value: json!("Vor jedem Umzug über 30 m³ zuerst eine Besichtigung anbieten."),
            confidence,
            evidence: "Alex hat das zweimal korrigiert".into(),
        }
    }

    #[test]
    fn parses_the_array_out_of_a_chatty_reply() {
        let reply = "Hier meine Analyse:\n```json\n[{\"kind\":\"rule\",\"scope\":\"global\",\"key\":\"besichtigung_ab_30m3\",\"value\":\"x\",\"confidence\":0.8}]\n```";
        let l = parse_lessons(reply);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].key, "besichtigung_ab_30m3");
        assert!(parse_lessons("Keine Lehren.").is_empty());
        assert!(parse_lessons("[kaputt").is_empty());
    }

    #[test]
    fn rejects_malformed_or_unsure_lessons() {
        assert!(is_acceptable(&lesson("rule", "global", "k", 0.8)));
        assert!(!is_acceptable(&lesson("rule", "global", "k", 0.4)), "too unsure");
        assert!(!is_acceptable(&lesson("opinion", "global", "k", 0.9)), "unknown kind");
        assert!(!is_acceptable(&lesson("rule", "customer:Frau Lange", "k", 0.9)), "scope must carry a real ID");
        assert!(is_acceptable(&lesson("fact", &format!("customer:{}", Uuid::now_v7()), "k", 0.9)));
        assert!(!is_acceptable(&lesson("rule", "global", "", 0.9)));
    }

    #[test]
    fn proposal_text_reads_naturally() {
        let t = proposal_text(&lesson("rule", "global", "k", 0.8));
        assert!(t.starts_with("🧠 Soll ich mir das merken?"));
        assert!(t.contains("(Regel, allgemein)"));
        assert!(t.contains("Grund: Alex hat das zweimal korrigiert"));
    }

    /// End to end against the DB: a correction in the chat becomes ONE pending
    /// proposal — never a stored memory — and the same lesson is not proposed twice.
    #[tokio::test]
    async fn a_correction_becomes_a_pending_proposal_not_a_memory() {
        let Ok(url) = std::env::var("DATABASE_URL") else { return };
        let Ok(pool) = sqlx::PgPool::connect(&url).await else { return };

        let key = format!("test_besichtigung_{}", Uuid::now_v7().simple());
        let episode = episodic::append(
            &pool,
            "Vorschlag schedule_inquiry(...) → von Alex abgebrochen — NICHT ausgeführt",
            &["schedule_inquiry".to_string(), "confirmation".to_string()],
            json!({}),
        )
        .await
        .unwrap();
        let llm = MockAssistantLlm::always(format!(
            "[{{\"kind\":\"rule\",\"scope\":\"global\",\"key\":\"{key}\",\"value\":\"Erst Besichtigung, dann Termin.\",\"confidence\":0.85,\"evidence\":\"Abbruch + Korrektur\"}}]"
        ));

        let first = reflect_on_last_day(&pool, &llm).await.unwrap();
        assert_eq!(first.iter().filter(|q| q.lesson.key == key).count(), 1);
        let stored = durable::recall(&pool, Some("global"), None).await.unwrap();
        assert!(!stored.iter().any(|m| m.key == key), "nothing is stored without Alex's tap");

        let second = reflect_on_last_day(&pool, &llm).await.unwrap();
        assert!(!second.iter().any(|q| q.lesson.key == key), "an open proposal is not repeated");

        sqlx::query("DELETE FROM pending_memory_proposals WHERE key = $1").bind(&key).execute(&pool).await.ok();
        sqlx::query("DELETE FROM agent_episodes WHERE id = $1").bind(episode).execute(&pool).await.ok();
    }
}
