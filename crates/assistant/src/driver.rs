//! Assistant driver loop.
//!
//! This is the main entry point for processing a Telegram message:
//!
//! 1. Receive normalised `Input { text, chat_id, images, quoted_text }`
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
    /// Base64-encoded images attached to this message — photos sent by Alex and
    /// rasterized PDF pages. Empty for plain text messages. Forwarded to the
    /// vision-capable model on the first user turn.
    pub images: Vec<String>,
    /// Text of the message Alex replied to / quoted in Telegram, when this turn
    /// is a reply. Carries the referent — e.g. a bot notification holding the
    /// inquiry/customer ID — that the bare `text` ("Erinnerung für diese Anfrage
    /// setzen") leaves implicit. Folded into the user turn, the retrieval query,
    /// and the grounding set so the assistant resolves the *correct* entity.
    pub quoted_text: Option<String>,
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

/// How many times one turn may be bounced back to the model for citing an
/// ungrounded ID before we stop relaying its (still fabricated) reply.
const MAX_GROUNDING_CORRECTIONS: usize = 1;

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

    // When this turn is a Telegram reply, the quoted message holds the referent
    // (often a bot notification with the inquiry/customer ID). Render it as a
    // labelled block so the model — and the retrieval/grounding steps below —
    // resolve "diese Anfrage" to the entity Alex actually pointed at, instead of
    // guessing a different customer (the Frederike mis-reminder).
    let quoted_block = input.quoted_text.as_deref().map(|q| {
        format!("[Zitierte Nachricht, auf die sich der Nutzer bezieht]\n{q}\n[Ende Zitat]")
    });

    // Step 3: Assemble memory bundle using the session's accumulated entity scopes (S4).
    // `active_scopes` grows as tools reference entity IDs (inquiry_id, customer_id, etc.)
    // and is persisted across Telegram message turns via the session row. The quoted
    // message is folded into the retrieval query so its IDs/names drive recall too.
    let scope_refs: Vec<&str> = session.active_scopes.iter().map(String::as_str).collect();
    let retrieval_query = match quoted_block.as_deref() {
        Some(q) => format!("{q}\n{}", input.text),
        None => input.text.clone(),
    };
    let bundle = retrieval::assemble_bundle(pool, llm.as_ref(), &retrieval_query, &scope_refs)
        .await
        .unwrap_or_default();
    debug!(load_log = ?bundle.load_log, "Memory bundle assembled");

    // Step 4: Build messages for the LLM.
    let memory_context = bundle.as_context_text();
    let system_prompt = build_system_prompt(soul, &memory_context);
    let mut messages = vec![aust_llm_providers::LlmMessage::system(system_prompt)];

    // Inject session history.
    let ctx_text = session.context_text();

    // Grounding guard: collect every entity ID the model is allowed to cite — those
    // present in the user's message, the conversation history, and recalled memory.
    // Successful tool results add more IDs below. Any UUID-shaped token in the final
    // reply that is NOT in this set is fabricated (the model claiming an action it
    // never took), and we refuse to relay it. See `extract_uuid_shapes`.
    let mut grounded_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for src in [
        input.text.as_str(),
        quoted_block.as_deref().unwrap_or(""),
        ctx_text.as_str(),
        memory_context.as_str(),
    ] {
        grounded_ids.extend(extract_uuid_shapes(src));
    }
    let mut grounding_corrections = 0usize;
    // How many tools actually ran (executed or got enqueued for confirmation) this
    // turn. If the model finalises a reply claiming a completed write while this is
    // still zero, it fabricated the action without calling any tool — the failure
    // mode behind the "Erinnerung gesetzt, aber nichts passiert" incident.
    let mut tools_acted_this_turn = 0usize;
    if !ctx_text.is_empty() {
        messages.push(aust_llm_providers::LlmMessage::user(format!(
            "[Gesprächsverlauf]\n{ctx_text}"
        )));
        messages.push(aust_llm_providers::LlmMessage::assistant(
            "Verstanden. Ich setze das Gespräch fort.".to_string(),
        ));
    }

    // Prepend the quoted message (if any) so the model sees exactly what Alex
    // replied to before his instruction.
    let user_turn_text = match quoted_block.as_deref() {
        Some(q) => format!("{q}\n\n{}", input.text),
        None => input.text.clone(),
    };

    if input.images.is_empty() {
        messages.push(aust_llm_providers::LlmMessage::user(user_turn_text));
    } else {
        // Vision turn: attach the photos / rasterized PDF pages to the user message.
        info!(chat_id = input.chat_id, image_count = input.images.len(), "Turn carries images");
        messages.push(aust_llm_providers::LlmMessage::user_with_images(
            user_turn_text,
            input.images.clone(),
        ));
    }

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
                // Grounding guard: catch IDs the model fabricated (claiming a write —
                // a filed report, a created record — that no tool actually performed).
                let ungrounded = ungrounded_uuids(&text, &grounded_ids);
                // Phantom-write guard: the reply claims a completed write (e.g. a set
                // reminder) but not a single tool ran this turn — the model never
                // actually performed it. This catches fabrications that cite no UUID
                // and so slip past the ID-based grounding check above.
                let phantom_write = tools_acted_this_turn == 0 && claims_completed_write(&text);
                if !ungrounded.is_empty() || phantom_write {
                    if grounding_corrections < MAX_GROUNDING_CORRECTIONS {
                        grounding_corrections += 1;
                        let correction = if !ungrounded.is_empty() {
                            warn!(
                                ?ungrounded,
                                "Reply cites ungrounded IDs — forcing honesty correction"
                            );
                            format!(
                                "[Grounding-Stopp] Deine letzte Antwort nennt ID(s), die durch KEIN \
                                 tatsächlich ausgeführtes Tool in diesem Zug belegt sind: {}. Du hast \
                                 diese Aktion NICHT ausgeführt. Erfinde niemals IDs oder Erfolgsmeldungen. \
                                 Wenn die Aktion gewünscht ist, rufe JETZT das passende Tool auf und warte \
                                 auf das echte Ergebnis. Andernfalls sag ehrlich, dass du es nicht getan \
                                 hast — ganz ohne erfundene ID.",
                                ungrounded.join(", ")
                            )
                        } else {
                            warn!("Reply claims a completed write but no tool ran — forcing honesty correction");
                            "[Grounding-Stopp] Deine Antwort behauptet eine erledigte Aktion (z. B. eine \
                             gesetzte Erinnerung, eine gespeicherte Änderung oder eine gesendete E-Mail), \
                             aber in diesem Zug wurde KEIN Tool ausgeführt. Du hast nichts gespeichert. \
                             Bestätige niemals eine Aktion, die du nicht über ein Tool ausgeführt hast. \
                             Rufe JETZT das passende Tool auf (z. B. set_reminder) und warte auf das echte \
                             Ergebnis, oder sag ehrlich, dass du es noch nicht getan hast."
                                .to_string()
                        };
                        messages.push(aust_llm_providers::LlmMessage::assistant(text.clone()));
                        messages.push(aust_llm_providers::LlmMessage::user(correction));
                        continue;
                    }
                    // Correction budget spent and still fabricating: refuse to relay it.
                    warn!(
                        ?ungrounded,
                        phantom_write,
                        "Reply still claims an unbacked action after correction — replacing with honest fallback"
                    );
                    reply = "Ich habe diese Aktion nicht nachweislich ausgeführt und kann sie \
                             daher nicht bestätigen. Sag mir, ob ich es (erneut) versuchen soll."
                        .to_string();
                    break;
                }
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

                        tools_acted_this_turn += 1;
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
                    tools_acted_this_turn += 1;

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

                            // Every ID returned by a real tool call is now citable.
                            grounded_ids.extend(extract_uuid_shapes(&result.to_string()));

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

/// Heuristic: does this reply assert that a write/side-effect was just completed?
///
/// Used only as a tripwire when *no* tool ran in the turn (see the phantom-write
/// guard in `process_turn`). Because the zero-tools gate already establishes that
/// the model called nothing, any first-person completion claim here is necessarily
/// a fabrication. The phrases below are the common German confirmations the model
/// emits for reminders, saves, sends and schedule changes. A read-summary that
/// merely mentions e.g. "angelegt am …" never reaches this check, because reading
/// requires a tool call (`tools_acted_this_turn > 0`).
pub(crate) fn claims_completed_write(text: &str) -> bool {
    let t = text.to_lowercase();
    const CLAIMS: [&str; 22] = [
        "erinnere dich",
        "erinnerung gesetzt",
        "erinnerung angelegt",
        "erinnerung eingerichtet",
        "erinnerung erstellt",
        "erinnerung ist gesetzt",
        "erinnerung wurde",
        "habe dich erinnert",
        "habe die erinnerung",
        "habe ich angelegt",
        "habe ich erstellt",
        "habe ich gesetzt",
        "habe ich gespeichert",
        "habe ich aktualisiert",
        "habe ich eingetragen",
        "habe ich hinterlegt",
        "habe ich versendet",
        "habe ich gesendet",
        "habe ich storniert",
        "habe ich abgeschaltet",
        "ist erledigt",
        "habe ich erledigt",
    ];
    CLAIMS.iter().any(|c| t.contains(c))
}

/// Extract all UUID-shaped tokens (8-4-4-4-12 alphanumeric groups) from text,
/// lowercased. Deliberately matches *alphanumeric* groups, not strict hex, so it
/// also catches malformed fabrications like `b8c5d9f3-2e4a-5f6g-0b9c-3d4e5f6g7a8c`
/// (note the invalid `g`) that a strict UUID parser would silently skip.
pub(crate) fn extract_uuid_shapes(s: &str) -> Vec<String> {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let bytes = s.as_bytes();
    let n = bytes.len();
    let is_an = |b: u8| b.is_ascii_alphanumeric();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let mut pos = i;
        let mut matched = true;
        for (gi, &glen) in GROUPS.iter().enumerate() {
            let mut k = 0;
            while k < glen && pos < n && is_an(bytes[pos]) {
                pos += 1;
                k += 1;
            }
            if k != glen {
                matched = false;
                break;
            }
            if gi < GROUPS.len() - 1 {
                if pos < n && bytes[pos] == b'-' {
                    pos += 1;
                } else {
                    matched = false;
                    break;
                }
            }
        }
        // Require clean boundaries so a longer run isn't partially matched.
        let prev_ok = i == 0 || !(is_an(bytes[i - 1]) || bytes[i - 1] == b'-');
        let next_ok = pos >= n || !(is_an(bytes[pos]) || bytes[pos] == b'-');
        if matched && prev_ok && next_ok {
            out.push(s[i..pos].to_ascii_lowercase());
            i = pos;
        } else {
            i += 1;
        }
    }
    out
}

/// Return the deduplicated UUID-shaped tokens in `text` that are not present in
/// `grounded` (i.e. not backed by user input, history, memory, or a tool result).
pub(crate) fn ungrounded_uuids(
    text: &str,
    grounded: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut seen = Vec::new();
    for u in extract_uuid_shapes(text) {
        if !grounded.contains(&u) && !seen.contains(&u) {
            seen.push(u);
        }
    }
    seen
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

    /// Grounding: a real UUID in backticks is extracted (lowercased).
    #[test]
    fn extract_uuid_shapes_finds_real_uuid() {
        let out = extract_uuid_shapes("Report `37BEE26C-412B-4DBE-A70F-1684FC0831C2` erfasst.");
        assert_eq!(out, vec!["37bee26c-412b-4dbe-a70f-1684fc0831c2"]);
    }

    /// Grounding: the malformed fabrication (invalid hex `g`) is still caught,
    /// because matching is alphanumeric-shaped, not strict hex.
    #[test]
    fn extract_uuid_shapes_catches_malformed_fabrication() {
        let out = extract_uuid_shapes("ID b8c5d9f3-2e4a-5f6g-0b9c-3d4e5f6g7a8c geschlossen");
        assert_eq!(out, vec!["b8c5d9f3-2e4a-5f6g-0b9c-3d4e5f6g7a8c"]);
    }

    /// Grounding: plain prose with no IDs yields nothing.
    #[test]
    fn extract_uuid_shapes_ignores_prose() {
        assert!(extract_uuid_shapes("Alles erledigt, kein Problem.").is_empty());
    }

    /// Grounding: an ID present in the grounded set is allowed; a fabricated one is flagged.
    #[test]
    fn ungrounded_uuids_flags_only_unbacked_ids() {
        let mut grounded = std::collections::HashSet::new();
        grounded.insert("37bee26c-412b-4dbe-a70f-1684fc0831c2".to_string());
        let text = "Echt: 37bee26c-412b-4dbe-a70f-1684fc0831c2, erfunden: 7a4f8c2e-1d3b-4e5f-9a8b-2c3d4e5f6a7b.";
        let out = ungrounded_uuids(text, &grounded);
        assert_eq!(out, vec!["7a4f8c2e-1d3b-4e5f-9a8b-2c3d4e5f6a7b"]);
    }

    /// Phantom-write: the exact fabrications from the reminder incident are caught.
    #[test]
    fn claims_completed_write_flags_reminder_confirmations() {
        assert!(claims_completed_write("Erledigt! Ich erinnere dich morgen um 12 Uhr ans Mittagessen."));
        assert!(claims_completed_write("Die Erinnerung wurde für 12:00 Uhr angelegt."));
        assert!(claims_completed_write("Habe ich gespeichert."));
        assert!(claims_completed_write("Erinnerung gesetzt ✅"));
    }

    /// Phantom-write: questions, plans and read-summaries are NOT flagged.
    #[test]
    fn claims_completed_write_ignores_non_claims() {
        assert!(!claims_completed_write("Soll ich dich um 12 Uhr erinnern?"));
        assert!(!claims_completed_write("Ich richte die Erinnerung jetzt ein …"));
        assert!(!claims_completed_write("Der Auftrag hat 3 offene Positionen."));
        assert!(!claims_completed_write("Wie kann ich helfen?"));
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
