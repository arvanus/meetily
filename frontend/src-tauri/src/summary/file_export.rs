use crate::database::repositories::meeting::MeetingsRepository;
use sqlx::SqlitePool;
use std::path::Path;
use tracing::{error, info, warn};

/// Saves the meeting summary as a `summary.md` file in the recording folder.
///
/// Uses the same format as the "Copy Summary" button in the frontend:
/// header with title, metadata (ID, date), separator, and markdown content.
///
/// Called both after AI generation and after manual edits.
pub async fn save_summary_to_folder(
    pool: &SqlitePool,
    meeting_id: &str,
    summary_json: &serde_json::Value,
) {
    let meeting = match MeetingsRepository::get_meeting_metadata(pool, meeting_id).await {
        Ok(Some(m)) => m,
        Ok(None) => {
            warn!("Cannot export summary.md: meeting not found for {}", meeting_id);
            return;
        }
        Err(e) => {
            error!("Cannot export summary.md: failed to fetch meeting {}: {}", meeting_id, e);
            return;
        }
    };

    let folder_path = match &meeting.folder_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            info!("No recording folder for meeting {}, skipping summary.md export", meeting_id);
            return;
        }
    };

    let folder = Path::new(&folder_path);
    if !folder.exists() {
        warn!("Recording folder does not exist for meeting {}: {}", meeting_id, folder_path);
        return;
    }

    let markdown_content = match summary_json.get("markdown").and_then(|v| v.as_str()) {
        Some(md) if !md.is_empty() => md,
        _ => {
            warn!("No markdown content to export for meeting {}", meeting_id);
            return;
        }
    };

    let title = &meeting.title;
    let date = meeting.created_at.0.format("%B %d, %Y %I:%M %p").to_string();
    let updated = chrono::Utc::now().format("%B %d, %Y %I:%M %p").to_string();

    let full_markdown = format!(
        "# Meeting Summary: {}\n\n\
         **Meeting ID:** {}\n\
         **Date:** {}\n\
         **Last updated:** {}\n\n\
         ---\n\n\
         {}",
        title, meeting_id, date, updated, markdown_content
    );

    let summary_path = folder.join("summary.md");
    match std::fs::write(&summary_path, &full_markdown) {
        Ok(()) => {
            info!("Exported summary.md to {} for meeting {}", summary_path.display(), meeting_id);
        }
        Err(e) => {
            error!("Failed to write summary.md to {}: {}", summary_path.display(), e);
        }
    }
}
