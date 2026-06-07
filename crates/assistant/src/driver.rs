//! Assistant driver loop.
//!
//! This is the main entry point for processing a Telegram message:
//!
//! 1. Receive normalised `Input { text, chat_id }`
//! 2. Resolve binding → role (reject unbound chats)
//! 3. Load / create session
//! 4. Assemble prompt (SOUL + memory bundle + tools preamble + history + user message)
//! 5. Call LLM with role-filtered tool schemas
//! 6. Iterate: execute Read tools, enqueue Confirm tools, validate args (one retry on failure)
//! 7. Persist turn + write audit rows for every tool call
//!
//! Note: the module is named `driver` (not `loop`) because `loop` is a reserved
//! keyword in Rust.

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use sqlx::PgPool;
use tracing::{debug, info, warn};

use crate::audit::{self, AuditEntry};
use crate::bindings;
use crate::confirmation::{self, Resolution};
use crate::error::{AssistantError, Result};
use crate::llm::{AssistantLlmProvider, ChatResponse, ModelTier};
use crate::memory::retrieval;
use crate::roles::Role;
use crate::session::{self, Turn};
use crate::soul::Soul;
use crate::tools::{Safety, ToolCtx, ToolRegistry};

/// Normalised input from a Telegram message.
pub struct Input {
    /// The text of the message (already transcribed if voice).
    pub text: String,
    /// Telegram chat ID.
    pub chat_id: i64,
}

/// The result of processing one input turn.
pub struct TurnResult {
    /// The final text response to send back to Telegram.
    pub reply: String,
    /// Whether a pending action was enqueued (session is now waiting for confirmation).
    pub awaiting_confirmation: bool,
    /// The ID of the pending action, if any.
    pub pending_action_id: Option<uuid::Uuid>,
    /// German summary of the proposed action, populated when `awaiting_confirmation`
    /// is true. The Telegram bridge uses this as the keyboard message body instead
    /// of `reply` so Alex sees a concrete action ("Rechnung … an … senden?") rather
    /// than the generic "Soll ich 'send_invoice' wirklich ausführen?" placeholder.
    pub pending_summary_de: Option<String>,
}

/// Maximum tool-calling iterations per turn (prevents infinite loops).
const MAX_TOOL_ITERATIONS: usize = 10;

/// Process one incoming Telegram message and produce a reply.
pub async fn process_turn(
    pool: &PgPool,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    soul: &Soul,
    services: aust_core::services::ServiceBundle,
    input: Input,
) -> Result<TurnResult> {
    // Step 1: Resolve binding.
    let binding = bindings::resolve(pool, input.chat_id).await?;
    let role = binding.role;
    info!(
        chat_id = input.chat_id,
        role = %role,
        "Processing turn"
    );

    // Step 2: Load or create session.
    let mut session = session::load_or_create(pool, input.chat_id).await?;
    let session_id = session.id;

    // Step 3: Assemble memory bundle using the session's accumulated entity scopes (S4).
    // `active_scopes` grows as tools reference entity IDs (inquiry_id, customer_id, etc.)
    // and is persisted across Telegram message turns via the session row.
    let scope_refs: Vec<&str> = session.active_scopes.iter().map(String::as_str).collect();
    let bundle = retrieval::assemble_bundle(pool, llm.as_ref(), &input.text, &scope_refs)
        .await
        .unwrap_or_default();
    debug!(load_log = ?bundle.load_log, "Memory bundle assembled");

    // Step 4: Build messages for the LLM.
    let system_prompt = build_system_prompt(soul, &bundle.as_context_text());
    let mut messages = vec![aust_llm_providers::LlmMessage::system(system_prompt)];

    // Inject session history.
    let ctx_text = session.context_text();
    if !ctx_text.is_empty() {
        messages.push(aust_llm_providers::LlmMessage::user(format!(
            "[Gesprächsverlauf]\n{ctx_text}"
        )));
        messages.push(aust_llm_providers::LlmMessage::assistant(
            "Verstanden. Ich setze das Gespräch fort.".to_string(),
        ));
    }

    messages.push(aust_llm_providers::LlmMessage::user(input.text.clone()));

    // Step 5: Tool-calling iteration loop.
    let tool_schemas = registry.schemas_for_role(role);
    let mut reply = String::new();
    let mut awaiting_confirmation = false;
    let mut pending_action_id: Option<uuid::Uuid> = None;
    let mut pending_summary_de: Option<String> = None;

    for iteration in 0..MAX_TOOL_ITERATIONS {
        let response = llm
            .chat_with_tools(ModelTier::Main, &messages, &tool_schemas)
            .await?;

        match response {
            ChatResponse::Text(text) => {
                reply = text;
                break;
            }
            ChatResponse::ToolCalls(calls) => {
                for call in calls {
                    debug!(tool = call.name, "LLM requested tool call");

                    // S4: Extract entity scopes from tool call arguments.
                    // When the LLM calls a tool with an inquiry_id or customer_id,
                    // add the corresponding scope so scoped memories are included
                    // in subsequent Telegram message turns. Persisted in session.
                    let new_scopes = extract_scopes_from_args(&call.arguments, &session.active_scopes, 5);
                    session.active_scopes.extend(new_scopes);

                    // Look up tool in registry (role-filtered).
                    let tool = match registry.get(&call.name, role) {
                        Some(t) => t,
                        None => {
                            warn!(tool = call.name, "Tool not found or not allowed for role");
                            messages.push(aust_llm_providers::LlmMessage::user(format!(
                                "[Tool-Fehler] Tool '{}' nicht verfügbar.",
                                call.name
                            )));
                            continue;
                        }
                    };

                    // Validate args (one retry on schema failure).
                    let validated_args = match ToolRegistry::validate_args(tool, &call.arguments) {
                        Ok(_) => call.arguments.clone(),
                        Err(e) => {
                            warn!(err = %e, "Tool arg validation failed — asking LLM to retry");
                            messages.push(aust_llm_providers::LlmMessage::user(format!(
                                "[Validierungsfehler] {e}. Bitte korrigiere die Argumente."
                            )));
                            // One retry: ask the LLM to fix its arguments.
                            let retry_resp = llm
                                .chat_with_tools(ModelTier::Main, &messages, &tool_schemas)
                                .await?;
                            match retry_resp {
                                ChatResponse::ToolCalls(retry_calls) => {
                                    if let Some(rc) = retry_calls.into_iter().find(|c| c.name == call.name) {
                                        rc.arguments
                                    } else {
                                        continue;
                                    }
                                }
                                ChatResponse::Text(t) => {
                                    reply = t;
                                    break;
                                }
                            }
                        }
                    };

                    // For Confirm-safety tools, always enqueue.
                    // For Write-safety tools, enqueue when role is Operator (Owner executes immediately).
                    let needs_confirmation = matches!(tool.safety(), Safety::Confirm)
                        || (matches!(tool.safety(), Safety::Write) && role == Role::Operator);

                    let start = Instant::now();

                    if needs_confirmation {
                        let pending_id = confirmation::enqueue_with_chat(
                            pool,
                            session_id,
                            &call.name,
                            &validated_args,
                            None, // Telegram message ID set later by the caller.
                            Some(input.chat_id),
                        )
                        .await?;

                        audit::record(
                            pool,
                            AuditEntry {
                                session_id,
                                tool_name: &call.name,
                                args: &validated_args,
                                result: None,
                                error_message: Some("awaiting confirmation"),
                                duration_ms: Some(start.elapsed().as_millis() as i32),
                                confirmed_action_id: Some(pending_id),
                            },
                        )
                        .await
                        .unwrap_or_else(|e| warn!("Audit write failed: {e}"));

                        let summary = tool.summarize(&validated_args);
                        pending_action_id = Some(pending_id);
                        awaiting_confirmation = true;
                        reply = summary.clone();
                        pending_summary_de = Some(summary);
                        break;
                    }

                    // Execute the tool.
                    let ctx = ToolCtx {
                        db: pool.clone(),
                        llm: llm.clone(),
                        services: services.clone(),
                        role,
                        user_id: binding.user_id,
                        chat_id: input.chat_id,
                        session_id,
                        confirmed: false,
                    };

                    let exec_result = tool.execute(&ctx, &validated_args).await;
                    let duration_ms = start.elapsed().as_millis() as i32;

                    match exec_result {
                        Ok(result) => {
                            audit::record(
                                pool,
                                AuditEntry {
                                    session_id,
                                    tool_name: &call.name,
                                    args: &validated_args,
                                    result: Some(&result),
                                    error_message: None,
                                    duration_ms: Some(duration_ms),
                                    confirmed_action_id: None,
                                },
                            )
                            .await
                            .unwrap_or_else(|e| warn!("Audit write failed: {e}"));

                            // Feed the tool result back into the conversation.
                            messages.push(aust_llm_providers::LlmMessage::user(format!(
                                "[Tool-Ergebnis: {}]\n{}",
                                call.name, result
                            )));
                        }
                        Err(e) => {
                            warn!(tool = call.name, err = %e, "Tool execution failed");
                            let err_str = e.to_string();
                            audit::record(
                                pool,
                                AuditEntry {
                                    session_id,
                                    tool_name: &call.name,
                                    args: &validated_args,
                                    result: None,
                                    error_message: Some(&err_str),
                                    duration_ms: Some(duration_ms),
                                    confirmed_action_id: None,
                                },
                            )
                            .await
                            .unwrap_or_else(|e| warn!("Audit write failed: {e}"));

                            messages.push(aust_llm_providers::LlmMessage::user(format!(
                                "[Tool-Fehler: {}] {err_str}",
                                call.name
                            )));
                        }
                    }
                }

                if awaiting_confirmation || iteration == MAX_TOOL_ITERATIONS - 1 {
                    break;
                }
            }
        }
    }

    if reply.is_empty() {
        reply = "Entschuldigung, ich konnte keine Antwort generieren.".to_string();
    }

    // Step 7: Persist the turn.
    session::append_turn(
        pool,
        &mut session,
        Turn::user(input.text),
        llm.as_ref(),
    )
    .await
    .unwrap_or_else(|e| warn!("Failed to persist user turn: {e}"));

    session::append_turn(
        pool,
        &mut session,
        Turn::assistant(reply.clone()),
        llm.as_ref(),
    )
    .await
    .unwrap_or_else(|e| warn!("Failed to persist assistant turn: {e}"));

    Ok(TurnResult {
        reply,
        awaiting_confirmation,
        pending_action_id,
        pending_summary_de,
    })
}

/// Parameters for resuming a confirmed pending action.
pub struct ResumeParams {
    pub pending_id: uuid::Uuid,
    pub resolution: Resolution,
    pub role: Role,
    pub user_id: uuid::Uuid,
    pub chat_id: i64,
}

/// Resume a confirmed pending action and return the tool result as a reply.
pub async fn resume_confirmed(
    pool: &PgPool,
    llm: Arc<dyn AssistantLlmProvider>,
    registry: &ToolRegistry,
    services: aust_core::services::ServiceBundle,
    params: ResumeParams,
) -> Result<Value> {
    let ResumeParams { pending_id, resolution, role, user_id, chat_id } = params;
    // Fetch the pending action.
    let pending = confirmation::fetch(pool, pending_id).await?;
    if pending.status != "pending" {
        return Err(AssistantError::PendingActionNotFound(pending_id));
    }

    // H3: validate the resuming chat owns the pending action so a different
    // Telegram chat cannot hijack a confirmation queued elsewhere. Falls back
    // to plain resolve when chat_id was not recorded at enqueue time (legacy rows).
    confirmation::resolve_from_chat(pool, pending_id, resolution.clone(), chat_id).await?;

    let args = match &resolution {
        Resolution::Confirmed => pending.proposed_args.clone(),
        Resolution::Edited(new_args) => new_args.clone(),
        Resolution::Canceled => {
            return Ok(serde_json::json!({ "status": "canceled" }));
        }
    };

    let tool = registry
        .get(&pending.tool_name, role)
        .ok_or_else(|| AssistantError::NotFound(pending.tool_name.clone()))?;

    let ctx = ToolCtx {
        db: pool.clone(),
        llm,
        services,
        role,
        user_id,
        chat_id,
        session_id: pending.session_id,
        // B1: the resume path is the only place `confirmed = true` is set.
        // Confirm-safety tools branch on this to perform their real side effect.
        confirmed: true,
    };

    let start = Instant::now();
    let result = tool.execute(&ctx, &args).await;
    let duration_ms = start.elapsed().as_millis() as i32;

    match result {
        Ok(v) => {
            audit::record(
                pool,
                AuditEntry {
                    session_id: pending.session_id,
                    tool_name: &pending.tool_name,
                    args: &args,
                    result: Some(&v),
                    error_message: None,
                    duration_ms: Some(duration_ms),
                    confirmed_action_id: Some(pending_id),
                },
            )
            .await
            .unwrap_or_else(|e| warn!("Audit write failed after confirmation: {e}"));
            Ok(v)
        }
        Err(e) => {
            let err_str = e.to_string();
            audit::record(
                pool,
                AuditEntry {
                    session_id: pending.session_id,
                    tool_name: &pending.tool_name,
                    args: &args,
                    result: None,
                    error_message: Some(&err_str),
                    duration_ms: Some(duration_ms),
                    confirmed_action_id: Some(pending_id),
                },
            )
            .await
            .unwrap_or_else(|e2| warn!("Audit write failed after confirmation error: {e2}"));
            Err(e)
        }
    }
}

/// Extract entity scopes from a tool-call argument map (S4 helper).
///
/// Returns scope strings like `"inquiry:<uuid>"`, `"customer:<uuid>"`, etc.
/// Caps at `max` to avoid blowing the memory bundle budget.
pub(crate) fn extract_scopes_from_args(args: &serde_json::Value, existing: &[String], max: usize) -> Vec<String> {
    let mut new_scopes = Vec::new();
    for (key, prefix) in [
        ("inquiry_id", "inquiry"),
        ("customer_id", "customer"),
        ("employee_id", "employee"),
    ] {
        if let Some(id_str) = args[key].as_str() {
            let scope = format!("{prefix}:{id_str}");
            if !existing.contains(&scope) && !new_scopes.contains(&scope)
                && existing.len() + new_scopes.len() < max
            {
                new_scopes.push(scope);
            }
        }
    }
    new_scopes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// S4: extract_scopes_from_args must add inquiry/customer/employee scopes from tool args.
    #[test]
    fn extract_scopes_adds_entity_scopes() {
        let existing = vec!["global".to_string()];
        let args = json!({ "inquiry_id": "018f5c30-0000-7000-8000-000000000001" });
        let new_scopes = extract_scopes_from_args(&args, &existing, 5);
        assert_eq!(new_scopes, vec!["inquiry:018f5c30-0000-7000-8000-000000000001"]);
    }

    /// S4: cap at max scopes.
    #[test]
    fn extract_scopes_caps_at_max() {
        let existing: Vec<String> = (0..4).map(|i| format!("inquiry:id-{i}")).collect();
        // 4 existing + "global" = 5, so we can add 0 more when max=5.
        let mut existing_with_global = vec!["global".to_string()];
        existing_with_global.extend(existing);
        let args = json!({ "inquiry_id": "new-id", "customer_id": "cust-id" });
        let new_scopes = extract_scopes_from_args(&args, &existing_with_global, 5);
        assert!(new_scopes.is_empty(), "should not exceed max scopes");
    }

    /// S4: duplicate scopes must not be added.
    #[test]
    fn extract_scopes_no_duplicates() {
        let id = "018f5c30-0000-7000-8000-000000000002";
        let existing = vec!["global".to_string(), format!("inquiry:{id}")];
        let args = json!({ "inquiry_id": id });
        let new_scopes = extract_scopes_from_args(&args, &existing, 10);
        assert!(new_scopes.is_empty(), "duplicate scope must not be added");
    }
}

fn build_system_prompt(soul: &Soul, memory_context: &str) -> String {
    let soul_text = soul.as_system_prompt();
    let now = current_datetime_context();
    let base = format!("{soul_text}\n\n---\n\n{now}");
    if memory_context.is_empty() {
        base
    } else {
        format!("{base}\n\n---\n\n{memory_context}")
    }
}

/// Current wall-clock context in Europe/Berlin, injected into every system prompt
/// so the assistant resolves relative dates ("heute", "kommende Woche") on its own
/// instead of asking the user.
fn current_datetime_context() -> String {
    use chrono::Datelike;
    use chrono_tz::Europe::Berlin;

    let now = chrono::Utc::now().with_timezone(&Berlin);
    let weekday = match now.weekday() {
        chrono::Weekday::Mon => "Montag",
        chrono::Weekday::Tue => "Dienstag",
        chrono::Weekday::Wed => "Mittwoch",
        chrono::Weekday::Thu => "Donnerstag",
        chrono::Weekday::Fri => "Freitag",
        chrono::Weekday::Sat => "Samstag",
        chrono::Weekday::Sun => "Sonntag",
    };
    format!(
        "# Aktuelles Datum und Uhrzeit\n\
         Heute ist {weekday}, der {}. Uhrzeit: {} (Zeitzone Europe/Berlin).\n\
         Nutze dieses Datum, um relative Angaben wie \"heute\", \"morgen\", \"diese Woche\" \
         oder \"kommende Woche\" selbst aufzulösen. Frage NICHT nach dem aktuellen Datum oder Jahr.",
        now.format("%d.%m.%Y"),
        now.format("%H:%M")
    )
}
