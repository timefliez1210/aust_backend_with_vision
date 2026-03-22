use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A general-purpose admin note.
///
/// **Caller**: Admin notepad endpoints.
/// **Why**: Lets admin users jot down freeform notes from within the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: Uuid,
    pub title: String,
    pub content: String,
    pub color: String,
    pub pinned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a note.
#[derive(Debug, Deserialize)]
pub struct CreateNote {
    pub title: Option<String>,
    pub content: Option<String>,
    pub color: Option<String>,
    pub pinned: Option<bool>,
}

/// Input for updating a note (all fields optional).
#[derive(Debug, Default, Deserialize)]
pub struct UpdateNote {
    pub title: Option<String>,
    pub content: Option<String>,
    pub color: Option<String>,
    pub pinned: Option<bool>,
}
