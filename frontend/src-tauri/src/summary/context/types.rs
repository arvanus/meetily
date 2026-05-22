use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Attachment shape exposed to the frontend (no content, no internal paths).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextAttachmentInfo {
    pub id: String,
    pub display_name: String,
    pub size_bytes: i64,
    pub truncated: bool,
    pub created_at: String,
}

/// Combined response from `api_get_summary_context`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryContextData {
    pub context_prompt: String,
    pub attachments: Vec<ContextAttachmentInfo>,
}

/// Full attachment row as stored in `meeting_context_attachments`.
#[derive(Debug, Clone, FromRow)]
pub struct ContextAttachmentRow {
    pub id: String,
    pub meeting_id: String,
    pub original_path: String,
    pub stored_filename: String,
    pub display_name: String,
    pub size_bytes: i64,
    pub truncated: bool,
    pub sort_order: i64,
    pub created_at: String,
}

/// Materialized attachment passed to the prompt builder.
#[derive(Debug, Clone)]
pub struct AttachmentContent {
    pub display_name: String,
    pub content: String,
    pub truncated: bool,
}
