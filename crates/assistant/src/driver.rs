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
use crate::memory::{episodic, retrieval};
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
    /// Entity IDs recorded for the replied-to bot message (`message_refs`). Exact,
    /// unlike `quoted_text`, which usually only names a customer.
    pub quoted_refs: Option<Value>,
    /// A confirmed action that ran just before this turn, outside it (Alex tapped
    /// Bestätigen). Set only on the continuation turn the bridge starts after a
    /// confirmation, so the guards count the action as done in this turn and
    /// the model can pick up the rest of the original task.
    pub prior_action: Option<PriorAction>,
}

/// A tool that already ran, carried into the next turn (see `Input::prior_action`).
pub struct PriorAction {
    pub tool_name: String,
    pub result: Value,
}

/// The result of processing one input turn.
pub struct TurnResult {
    /// The final text response to send back to Telegram.
    pub reply: String,
    /// Whether a pending action was enqueued (session is now waiting for confirmation).
    pub awaiting_confirmation: bool,
    /// The ID of the pending action, if any.
    pub pending_action_id: Option<uuid::Uuid>,
    /// Entity IDs this turn's tools acted on or returned — arguments first — so the
    /// bridge can record them with the posted reply (`message_refs`). A later reply
    /// to Josie's message then resolves "die" / "den Termin" to these exact IDs.
    pub cited_ids: Vec<String>,
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
    match session::roll_over_if_idle(pool, &mut session, llm.as_ref()).await {
        Ok(true) => info!(chat_id = input.chat_id, "Session rolled over after idle period"),
        Ok(false) => {}
        Err(e) => warn!(chat_id = input.chat_id, "Session rollover failed: {e}"),
    }

    // When this turn is a Telegram reply, the quoted message holds the referent
    // (often a bot notification with the inquiry/customer ID). Render it as a
    // labelled block so the model — and the retrieval/grounding steps below —
    // resolve "diese Anfrage" to the entity Alex actually pointed at, instead of
    // guessing a different customer (the Frederike mis-reminder).
    let refs_line = input
        .quoted_refs
        .as_ref()
        .map(crate::message_refs::render)
        .filter(|l| !l.is_empty());
    let quoted_block = match (input.quoted_text.as_deref(), refs_line.as_deref()) {
        (None, None) => None,
        (q, r) => {
            let mut block = String::from("[Zitierte Nachricht, auf die sich der Nutzer bezieht]\n");
            if let Some(q) = q {
                block.push_str(q);
                block.push('\n');
            }
            // The exact referent, from what the bot recorded when it sent that
            // message — authoritative over any name in the text.
            if let Some(r) = r {
                block.push_str(&format!(
                    "[Bezug laut System (verbindlich, nicht per Name suchen): {r}]\n"
                ));
            }
            block.push_str("[Ende Zitat]");
            Some(block)
        }
    };
    if let Some(refs) = &input.quoted_refs {
        // The quoted entities are this turn's subject: load their memories.
        touch_scopes(&mut session.active_scopes, refs, MAX_ACTIVE_SCOPES);
    }

    // Step 3: Assemble memory bundle using the session's accumulated entity scopes (S4).
    // `active_scopes` grows as tools reference entity IDs (inquiry_id, customer_id, etc.)
    // and is persisted across Telegram message turns via the session row. The quoted
    // message is folded into the retrieval query so its IDs/names drive recall too.
    let scope_refs: Vec<&str> = session.active_scopes.iter().map(String::as_str).collect();
    let retrieval_query = match quoted_block.as_deref() {
        Some(q) => format!("{q}\n{}", input.text),
        None => input.text.clone(),
    };
    let bundle = retrieval::assemble_bundle(pool, &retrieval_query, &scope_refs)
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
    // Names of the tools that actually *succeeded* this turn. The count above only
    // catches a turn where nothing ran at all; it misses the turn that runs some
    // tools and then claims a *different* write on top (the Besichtigung incident:
    // create_customer + create_inquiry ran, and the reply announced an appointment
    // that no tool ever created). `unbacked_claim` checks the claim against this set.
    let mut tools_ran_this_turn: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // One trace per tool call, persisted with the assistant turn (see `Turn::tools`).
    let mut traces: Vec<String> = Vec::new();
    // IDs for `TurnResult::cited_ids`, in the order the tools touched them.
    let mut cited_ids: Vec<String> = Vec::new();
    // Writes that went through this turn, kept as episodes once the turn is done.
    let mut episodes: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(prior) = &input.prior_action {
        grounded_ids.extend(extract_uuid_shapes(&prior.result.to_string()));
        tools_acted_this_turn += 1;
        tools_ran_this_turn.insert(prior.tool_name.clone());
    }
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
        messages.push(aust_llm_providers::LlmMessage::user(user_turn_text.clone()));
    } else {
        // Vision turn: attach the photos / rasterized PDF pages to the user message.
        info!(chat_id = input.chat_id, image_count = input.images.len(), "Turn carries images");
        messages.push(aust_llm_providers::LlmMessage::user_with_images(
            user_turn_text.clone(),
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
                // Unbacked-claim guard: the reply announces a completed write to some
                // entity (a Termin, an Angebot, a Rechnung …) but no tool that touches
                // that entity ran this turn — even though *other* tools did, which is
                // what lets it slip past the phantom-write gate above.
                let unbacked = unbacked_claim(&text, &tools_ran_this_turn);
                if !ungrounded.is_empty() || phantom_write || unbacked.is_some() {
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
                        } else if phantom_write {
                            warn!("Reply claims a completed write but no tool ran — forcing honesty correction");
                            "[Grounding-Stopp] Deine Antwort behauptet eine erledigte Aktion (z. B. eine \
                             gesetzte Erinnerung, eine gespeicherte Änderung oder eine gesendete E-Mail), \
                             aber in diesem Zug wurde KEIN Tool ausgeführt. Du hast nichts gespeichert. \
                             Bestätige niemals eine Aktion, die du nicht über ein Tool ausgeführt hast. \
                             Rufe JETZT das passende Tool auf (z. B. set_reminder) und warte auf das echte \
                             Ergebnis, oder sag ehrlich, dass du es noch nicht getan hast."
                                .to_string()
                        } else {
                            let subject = unbacked.unwrap_or("diese Aktion");
                            warn!(subject, "Reply claims a write no tool in this turn backs — forcing honesty correction");
                            format!(
                                "[Grounding-Stopp] Deine Antwort meldet '{subject}' als erledigt. In diesem Zug \
                                 wurde aber KEIN Tool ausgeführt, das '{subject}' anlegt, ändert oder auch nur \
                                 liest. Andere Tools aufzurufen genügt nicht — eine Anfrage anzulegen legt z. B. \
                                 KEINEN Termin an. Du hast '{subject}' nicht gespeichert. Rufe JETZT das passende \
                                 Tool auf (z. B. create_inquiry_appointment für eine Besichtigung) und warte auf \
                                 das echte Ergebnis, oder sag ehrlich, dass du es noch nicht getan hast."
                            )
                        };
                        messages.push(aust_llm_providers::LlmMessage::assistant(text.clone()));
                        messages.push(aust_llm_providers::LlmMessage::user(correction));
                        continue;
                    }
                    // Correction budget spent and still fabricating: refuse to relay it.
                    warn!(
                        ?ungrounded,
                        phantom_write,
                        unbacked,
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
                // Keep the model's own call in the transcript, so each result below
                // answers a request it can see it made.
                messages.push(aust_llm_providers::LlmMessage::assistant_tool_calls(
                    "",
                    calls
                        .iter()
                        .map(|c| aust_llm_providers::LlmToolCall {
                            name: c.name.clone(),
                            arguments: c.arguments.clone(),
                        })
                        .collect(),
                ));
                for call in calls {
                    debug!(tool = call.name, "LLM requested tool call");

                    // S4: Extract entity scopes from tool call arguments.
                    // When the LLM calls a tool with an inquiry_id or customer_id,
                    // add the corresponding scope so scoped memories are included
                    // in subsequent Telegram message turns. Persisted in session.
                    touch_scopes(&mut session.active_scopes, &call.arguments, MAX_ACTIVE_SCOPES);

                    // Look up tool in registry (role-filtered).
                    let tool = match registry.get(&call.name, role) {
                        Some(t) => t,
                        None => {
                            warn!(tool = call.name, "Tool not found or not allowed for role");
                            messages.push(aust_llm_providers::LlmMessage::tool_result(
                                &call.name,
                                format!("[Tool-Fehler] Tool '{}' nicht verfügbar.", call.name),
                            ));
                            continue;
                        }
                    };

                    // Invalid args are reported back as this call's result; the next
                    // iteration lets the model correct them with the error in view.
                    if let Err(e) = ToolRegistry::validate_args(tool, &call.arguments) {
                        warn!(tool = call.name, err = %e, "Tool arg validation failed");
                        messages.push(aust_llm_providers::LlmMessage::tool_result(
                            &call.name,
                            format!("[Validierungsfehler] {e}. Der Aufruf wurde nicht ausgeführt — bitte mit korrigierten Argumenten erneut aufrufen."),
                        ));
                        continue;
                    }
                    let validated_args = call.arguments.clone();

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
                        traces.push(session::tool_trace(
                            &call.name,
                            &validated_args,
                            &format!("wartet auf Bestätigung durch Alex (pending {pending_id})"),
                        ));
                        cite_arg_ids(&mut cited_ids, &validated_args);
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

                            // A tool that ran but could not do the job reports it in-band
                            // as {"ok": false, "message": ...} — no email on file, the
                            // reminder already closed, SMTP refused the address. Treating
                            // that as backing let "Erinnerung gesetzt" through for a
                            // reminder that was never created. It ran; it did not act.
                            let acted = result["ok"].as_bool() != Some(false);

                            // Every ID returned by a real tool call is now citable,
                            // and the tool itself now backs claims about its entity.
                            let result_text = result.to_string();
                            grounded_ids.extend(extract_uuid_shapes(&result_text));
                            traces.push(session::tool_trace(&call.name, &validated_args, &result_text));
                            cite_arg_ids(&mut cited_ids, &validated_args);
                            cited_ids.extend(extract_uuid_shapes(&result_text));
                            if acted && !matches!(tool.safety(), Safety::Read) {
                                episodes.push(write_episode(&input.text, &call.name, &validated_args, &result));
                            }
                            if acted {
                                tools_ran_this_turn.insert(call.name.clone());
                            } else {
                                warn!(
                                    tool = call.name,
                                    "Tool declined in-band; not counting it as backing a claim"
                                );
                            }

                            // Feed the tool result back into the conversation.
                            messages.push(aust_llm_providers::LlmMessage::tool_result(
                                &call.name,
                                cap_tool_result(&result.to_string()),
                            ));
                        }
                        Err(e) => {
                            warn!(tool = call.name, err = %e, "Tool execution failed");
                            let err_str = e.to_string();
                            traces.push(session::tool_trace(
                                &call.name,
                                &validated_args,
                                &format!("FEHLER: {err_str}"),
                            ));
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

                            messages.push(aust_llm_providers::LlmMessage::tool_result(
                                &call.name,
                                format!("[Tool-Fehler] {err_str}"),
                            ));
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
    // Persist what the model was actually shown, quote included. Storing the bare text
    // meant the referent was gone by the next turn: Alex replies to a customer's message
    // with "erinnere sie an den Termin", Josie handles it, and then "ja, mach das" has
    // no idea who "sie" is. That is the wrong-customer reminder, one turn later.
    session::append_turn(
        pool,
        &mut session,
        Turn::user(user_turn_text),
        llm.as_ref(),
    )
    .await
    .unwrap_or_else(|e| warn!("Failed to persist user turn: {e}"));

    session::append_turn(
        pool,
        &mut session,
        Turn::assistant(reply.clone()).with_tools(traces),
        llm.as_ref(),
    )
    .await
    .unwrap_or_else(|e| warn!("Failed to persist assistant turn: {e}"));

    // Remember what was done — in the background: with local embeddings on, the
    // first episode after a quiet spell loads the model, which must not hold up
    // the reply.
    if !episodes.is_empty() {
        let pool = pool.clone();
        aust_core::tenant::spawn(async move {
            for (summary, tags) in episodes {
                if let Err(e) = episodic::append(&pool, &summary, &tags, serde_json::json!({})).await {
                    warn!("Failed to store episode: {e}");
                }
            }
        });
    }

    Ok(TurnResult {
        reply,
        awaiting_confirmation,
        pending_action_id,
        cited_ids,
        pending_summary_de,
    })
}

/// One episode for a write a tool performed: what Alex asked, what ran, the
/// outcome. Tagged with the tool and the entities it touched, so it surfaces
/// when the conversation is about them again.
fn write_episode(request: &str, tool: &str, args: &Value, result: &Value) -> (String, Vec<String>) {
    let request: String = request.chars().take(160).collect();
    let args_text: String = args.to_string().chars().take(200).collect();
    let outcome = result["summary"]
        .as_str()
        .or(result["message"].as_str())
        .map(|s| s.chars().take(160).collect::<String>())
        .unwrap_or_else(|| "erledigt".to_string());
    let today = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Berlin).format("%d.%m.%Y");
    let summary = format!("{today}: Alex: „{request}“ → {tool}({args_text}) → {outcome}");
    (summary, episode_tags(tool, args))
}

/// Tags for an episode: the tool plus `inquiry:/customer:/employee:` scopes
/// from its arguments (the same strings as `AgentSession::active_scopes`).
fn episode_tags(tool: &str, args: &Value) -> Vec<String> {
    let mut tags = vec![tool.to_string()];
    touch_scopes(&mut tags, args, usize::MAX);
    tags
}

/// Record how Alex resolved a confirmation in the chat's session history.
///
/// The tap happens outside any driver turn, so without this the next message
/// had no idea whether the mail went out, the Termin was cancelled, or Alex
/// declined — "hast du sie angeschrieben?" was answered from a pending summary.
pub async fn record_resolution(pool: &PgPool, chat_id: i64, tool_name: &str, args: &Value, outcome: &str) {
    let turn = Turn::assistant(format!("[Bestätigungsknopf] {tool_name}: {outcome}"))
        .with_tools(vec![session::tool_trace(tool_name, args, outcome)]);
    if let Err(e) = session::append_note(pool, chat_id, turn).await {
        warn!(chat_id, tool_name, "Failed to record confirmation outcome in session: {e}");
    }
    // Also an episode: how Alex reacts to proposals (confirms, cancels, edits) is
    // the strongest signal of what he wants — the input for nightly reflection.
    let mut tags = episode_tags(tool_name, args);
    tags.push("confirmation".to_string());
    let today = chrono::Utc::now().with_timezone(&chrono_tz::Europe::Berlin).format("%d.%m.%Y");
    let args_text: String = args.to_string().chars().take(200).collect();
    let outcome_text: String = outcome.chars().take(200).collect();
    let summary = format!("{today}: Vorschlag {tool_name}({args_text}) → {outcome_text}");
    if let Err(e) = episodic::append(pool, &summary, &tags, serde_json::json!({})).await {
        warn!(chat_id, tool_name, "Failed to store confirmation episode: {e}");
    }
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
            let outcome = if v["ok"].as_bool() == Some(false) {
                format!("bestätigt, aber NICHT ausgeführt: {}", v["message"].as_str().unwrap_or("Grund unbekannt"))
            } else {
                format!("bestätigt und ausgeführt → {v}")
            };
            record_resolution(pool, chat_id, &pending.tool_name, &args, &outcome).await;
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
            record_resolution(pool, chat_id, &pending.tool_name, &args, &format!("bestätigt, aber FEHLER: {err_str}")).await;
            Err(e)
        }
    }
}

/// Push every `*_id` argument of a tool call onto `cited` (the entity it acts on).
fn cite_arg_ids(cited: &mut Vec<String>, args: &Value) {
    if let Some(obj) = crate::message_refs::from_payload(args).as_object() {
        cited.extend(obj.values().filter_map(Value::as_str).map(str::to_lowercase));
    }
}

/// Longest tool result fed back to the model, in characters. Inbox and calendar
/// listings can run to tens of kilobytes and crowd the conversation out of the
/// context window; past this the model is told to narrow its query.
const MAX_TOOL_RESULT_CHARS: usize = 8_000;

/// Cut an oversized tool result on a char boundary and say so in-band.
pub fn cap_tool_result(result: &str) -> String {
    if result.chars().count() <= MAX_TOOL_RESULT_CHARS {
        return result.to_string();
    }
    let kept: String = result.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    format!(
        "{kept}\n[… gekürzt: Ergebnis zu lang. Frag gezielter ab (Filter, Zeitraum, ID), \
         wenn dir hier etwas fehlt.]"
    )
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

/// Verbs asserting a side-effect has already happened.
const DONE_VERBS: [&str; 19] = [
    "angelegt",
    "eingetragen",
    "erstellt",
    "gesetzt",
    "gebucht",
    "eingeplant",
    "hinterlegt",
    "gespeichert",
    "aktualisiert",
    "geändert",
    "korrigiert",
    "storniert",
    "abgesagt",
    "verschoben",
    "versendet",
    "gesendet",
    "verschickt",
    "gelöscht",
    "zugewiesen",
];

/// A class of entity the assistant can claim to have written, and the tools whose
/// execution makes a claim about it credible.
struct ClaimClass {
    /// Nouns naming the entity.
    subjects: &'static [&'static str],
    /// Tools that write *or* read this entity. Writing proves the claim; reading is
    /// allowed too, because a reply may legitimately restate an entity a read tool
    /// just surfaced ("der Termin ist für den 16.07. eingetragen"). If neither ran,
    /// nothing in this turn could have told the model the entity exists.
    backing: &'static [&'static str],
}

/// Entity classes, checked in order. Deliberately coarse: this is a tripwire for
/// confident fabrication, not a proof system. A class that omits a legitimate
/// backing tool costs a wasted correction round-trip; a class that omits a *subject*
/// silently lets that fabrication through, so subjects err on the inclusive side.
const CLAIM_CLASSES: [ClaimClass; 6] = [
    // Termine — the Besichtigung incident (2026-07-13): Josie announced an
    // appointment for Frau Schilling after only create_customer + create_inquiry.
    ClaimClass {
        subjects: &["termin", "besichtigung", "kalender", "umzugstag"],
        backing: &[
            "create_inquiry_appointment",
            "create_calendar_item",
            "update_calendar_item",
            "delete_calendar_item",
            "schedule_inquiry",
            "reassign_termin",
            "cancel_termin",
            "get_calendar",
            "find_available_slots",
            "get_inquiry",
            "get_employee_assignments",
        ],
    },
    ClaimClass {
        subjects: &["erinnerung"],
        backing: &["set_reminder", "cancel_reminder", "list_reminders"],
    },
    ClaimClass {
        subjects: &["angebot"],
        backing: &[
            "recompute_offer",
            "apply_nl_override",
            "override_estimation",
            "send_offer_to_customer",
            "cancel_offer",
            "mark_offer_accepted",
            "mark_offer_rejected",
            "get_offer_history",
            "get_inquiry",
        ],
    },
    ClaimClass {
        subjects: &["rechnung"],
        backing: &[
            "create_invoice",
            "send_invoice",
            "update_invoice_status",
            "void_invoice",
            "record_payment",
            "send_payment_reminder",
            "get_invoice",
            "list_invoices",
            "list_invoice_reminders",
        ],
    },
    ClaimClass {
        subjects: &["e-mail", "email", "mail"],
        backing: &[
            "send_email",
            "draft_reply",
            "mark_email_handled",
            "categorize_email",
            "request_info_from_customer",
            "get_email",
            "list_inbox",
            "list_thread",
        ],
    },
    ClaimClass {
        subjects: &["crew", "mitarbeiter", "helfer"],
        backing: &[
            "set_inquiry_crew",
            "assign_employee",
            "set_employee_schedule",
            "set_employee_active",
            "update_employee",
            "get_assigned_crew",
            "get_employee",
            "get_employee_workload",
            "get_employee_assignments",
            "list_employees",
        ],
    },
];

/// Does the reply announce a completed write to an entity that no tool in this turn
/// touched? Returns the offending subject noun.
///
/// Complements the zero-tool phantom-write gate: this one fires on the turn that
/// *did* run tools, just not one that could substantiate what it claims. Subject and
/// verb must land in the same sentence, so a completed write in one sentence cannot
/// launder an unrelated noun in the next ("Anfrage angelegt. Für den Termin fehlt
/// noch ein Datum." must not trip the Termin class).
pub(crate) fn unbacked_claim(
    text: &str,
    ran: &std::collections::HashSet<String>,
) -> Option<&'static str> {
    let lower = text.to_lowercase();
    for sentence in lower.split(['.', '!', '?', '\n', ';']) {
        if !DONE_VERBS.iter().any(|v| sentence.contains(v)) {
            continue;
        }
        for class in &CLAIM_CLASSES {
            let Some(subject) = class.subjects.iter().find(|s| sentence.contains(**s)) else {
                continue;
            };
            if class.backing.iter().any(|b| ran.contains(*b)) {
                continue;
            }
            return Some(subject);
        }
    }
    None
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

/// Entity scopes kept on a session, `"global"` included.
const MAX_ACTIVE_SCOPES: usize = 5;

/// Mark the entities a tool call touches as the session's most recent scopes (S4).
///
/// Scopes are kept least-recently-used first. A touched scope moves to the end;
/// past `max` the oldest non-global scope is dropped. The previous version simply
/// stopped adding at the cap, and since a chat's session never ends, every chat
/// froze on the first four entities it ever saw — memories about any later
/// customer or inquiry were never loaded again.
pub(crate) fn touch_scopes(active: &mut Vec<String>, args: &serde_json::Value, max: usize) {
    for (key, prefix) in [
        ("inquiry_id", "inquiry"),
        ("customer_id", "customer"),
        ("employee_id", "employee"),
    ] {
        if let Some(id_str) = args[key].as_str() {
            let scope = format!("{prefix}:{id_str}");
            active.retain(|s| *s != scope);
            active.push(scope);
        }
    }
    while active.len() > max {
        match active.iter().position(|s| s != "global") {
            Some(i) => {
                active.remove(i);
            }
            None => break,
        }
    }
}

/// How to work with the tools and the Telegram context — the same for every
/// tenant, unlike the persona in SOUL.md.
const TOOLS_PREAMBLE: &str = include_str!("../prompts/tools_preamble.md");

fn build_system_prompt(soul: &Soul, memory_context: &str) -> String {
    let soul_text = soul.as_system_prompt();
    let now = current_datetime_context();
    let base = format!("{soul_text}\n\n---\n\n{TOOLS_PREAMBLE}\n\n---\n\n{now}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// S4: tool args add inquiry/customer/employee scopes.
    #[test]
    fn touch_scopes_adds_entity_scopes() {
        let mut active = vec!["global".to_string()];
        touch_scopes(&mut active, &json!({ "inquiry_id": "i1", "customer_id": "c1" }), 5);
        assert_eq!(active, vec!["global", "inquiry:i1", "customer:c1"]);
    }

    /// S4: at the cap the least recently used entity goes, never "global" —
    /// so a new customer still gets its memories loaded.
    #[test]
    fn touch_scopes_evicts_oldest_at_cap() {
        let mut active: Vec<String> = std::iter::once("global".to_string())
            .chain((0..4).map(|i| format!("inquiry:id-{i}")))
            .collect();
        touch_scopes(&mut active, &json!({ "customer_id": "new" }), 5);
        assert_eq!(active.len(), 5);
        assert_eq!(active[0], "global");
        assert!(!active.contains(&"inquiry:id-0".to_string()));
        assert_eq!(active.last().unwrap(), "customer:new");
    }

    /// S4: touching a known scope moves it to the end instead of duplicating it.
    #[test]
    fn touch_scopes_refreshes_without_duplicates() {
        let mut active = vec!["global".to_string(), "inquiry:a".to_string(), "inquiry:b".to_string()];
        touch_scopes(&mut active, &json!({ "inquiry_id": "a" }), 5);
        assert_eq!(active, vec!["global", "inquiry:b", "inquiry:a"]);
    }

    fn ran(tools: &[&str]) -> std::collections::HashSet<String> {
        tools.iter().map(|t| (*t).to_string()).collect()
    }

    /// The 2026-07-13 Besichtigung incident, replayed: Josie ran create_customer and
    /// create_inquiry, then announced an appointment no tool had created. Two writes
    /// had run, so the zero-tool phantom-write gate stayed silent.
    #[test]
    fn unbacked_claim_catches_fabricated_besichtigung() {
        let reply = "Besichtigungstermin für Frau Schilling angelegt.\n\n\
                     **Termin:** Do 16.07.2026, 14:00–15:00 Uhr";
        let tools = ran(&["search_customers", "create_customer", "create_inquiry"]);
        assert!(!claims_completed_write(reply), "no first-person phrase to catch");
        assert_eq!(unbacked_claim(reply, &tools), Some("termin"));
    }

    /// The same claim is fine once the appointment tool actually ran.
    #[test]
    fn unbacked_claim_accepts_backed_besichtigung() {
        let reply = "Besichtigungstermin für Frau Schilling angelegt.";
        let tools = ran(&["create_inquiry", "create_inquiry_appointment"]);
        assert_eq!(unbacked_claim(reply, &tools), None);
    }

    /// A read tool backs a restatement of an existing Termin — get_calendar surfaced
    /// it, so reporting it is grounded, not a fabricated write.
    #[test]
    fn unbacked_claim_allows_read_backed_restatement() {
        let reply = "Der Umzug Meier ist für den 16.07. eingetragen.";
        assert_eq!(unbacked_claim(reply, &ran(&["get_calendar"])), None);
    }

    /// Subject and verb must share a sentence: a real write must not launder an
    /// unrelated noun in the following sentence.
    #[test]
    fn unbacked_claim_requires_subject_and_verb_in_one_sentence() {
        let reply = "Anfrage angelegt. Für den Termin brauche ich noch ein Datum.";
        let tools = ran(&["create_inquiry"]);
        assert_eq!(unbacked_claim(reply, &tools), None);
    }

    /// A fabricated reminder is caught even though an unrelated tool ran.
    #[test]
    fn unbacked_claim_catches_fabricated_reminder() {
        let reply = "Erinnerung für heute 14:00 Uhr gesetzt.";
        assert_eq!(unbacked_claim(reply, &ran(&["get_calendar"])), Some("erinnerung"));
        assert_eq!(unbacked_claim(reply, &ran(&["set_reminder"])), None);
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
