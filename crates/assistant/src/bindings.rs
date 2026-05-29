//! Telegram chat binding repository.
//!
//! Maps Telegram `chat_id` → `(user_id, role)`. Any incoming message from an
//! unbound chat is rejected before the session is loaded.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AssistantError, Result};
use crate::roles::Role;

/// A resolved Telegram chat binding.
#[derive(Debug, Clone)]
pub struct ChatBinding {
    /// Internal user identifier.
    pub user_id: Uuid,
    /// Role that governs which tools are available in this session.
    pub role: Role,
}

/// Look up the binding for a Telegram chat ID.
///
/// Returns [`AssistantError::UnboundChat`] if no binding exists.
pub async fn resolve(pool: &PgPool, chat_id: i64) -> Result<ChatBinding> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT user_id, role FROM telegram_chat_bindings WHERE chat_id = $1")
            .bind(chat_id)
            .fetch_optional(pool)
            .await?;

    let (user_id, role_str) = row.ok_or(AssistantError::UnboundChat(chat_id))?;
    let role = Role::try_from(role_str.as_str())?;
    Ok(ChatBinding { user_id, role })
}

/// Insert or replace a chat binding (upsert).
///
/// Used during setup / re-assignment of an operator's chat.
pub async fn upsert(
    pool: &PgPool,
    chat_id: i64,
    user_id: Uuid,
    role: Role,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO telegram_chat_bindings (id, chat_id, user_id, role)
        VALUES (gen_random_uuid(), $1, $2, $3)
        ON CONFLICT (chat_id) DO UPDATE
            SET user_id = EXCLUDED.user_id,
                role    = EXCLUDED.role,
                updated_at = NOW()
        "#,
    )
    .bind(chat_id)
    .bind(user_id)
    .bind(role.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a chat binding (e.g. when an operator leaves).
pub async fn remove(pool: &PgPool, chat_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM telegram_chat_bindings WHERE chat_id = $1")
        .bind(chat_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // DB tests are in tests/integration.rs — unit tests here cover logic only.

    #[test]
    fn resolve_err_variant() {
        // Confirms that the error variant carries the chat_id.
        let e = AssistantError::UnboundChat(42);
        assert!(e.to_string().contains("42"));
    }
}
