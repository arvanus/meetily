//! Tauri commands for managing the per-meeting summary context.

use crate::state::AppState;
use crate::summary::context::repository::{
    ContextAttachmentsRepository, SummaryContextRepository,
};
use crate::summary::context::storage;
use crate::summary::context::types::{
    ContextAttachmentInfo, ContextAttachmentRow, SummaryContextData,
};
use chrono::Utc;
use log::{info as log_info, warn as log_warn};
use std::path::PathBuf;
use tauri::{AppHandle, Runtime};
use uuid::Uuid;

const MAX_ATTACHMENTS_PER_MEETING: i64 = 10;
const MAX_TOTAL_BYTES: i64 = 1024 * 1024;

#[tauri::command]
pub async fn api_save_summary_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    context_prompt: String,
) -> Result<(), String> {
    log_info!("api_save_summary_context for meeting_id: {}", &meeting_id);
    let pool = state.db_manager.pool();
    SummaryContextRepository::upsert_prompt(pool, &meeting_id, &context_prompt)
        .await
        .map_err(|e| format!("Failed to save context: {}", e))
}

#[tauri::command]
pub async fn api_get_summary_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<SummaryContextData, String> {
    let pool = state.db_manager.pool();
    let context_prompt = SummaryContextRepository::get_prompt(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load context: {}", e))?;
    let rows = ContextAttachmentsRepository::list(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load attachments: {}", e))?;
    let attachments = rows.iter().map(ContextAttachmentInfo::from).collect();
    Ok(SummaryContextData {
        context_prompt,
        attachments,
    })
}

/// Resolve `<meeting_folder>` from `meetings.folder_path`.
async fn resolve_meeting_folder(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
) -> Result<PathBuf, String> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("DB error: {}", e))?;
    let folder = row.and_then(|(p,)| p).ok_or_else(|| {
        "Meeting folder is not initialized \u{2014} start a recording at least once".to_string()
    })?;
    let p = PathBuf::from(folder);
    if !p.exists() {
        return Err(format!(
            "Meeting folder does not exist on disk: {}",
            p.display()
        ));
    }
    Ok(p)
}

#[tauri::command]
pub async fn api_add_context_attachment<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    source_path: String,
) -> Result<ContextAttachmentInfo, String> {
    let pool = state.db_manager.pool();

    // Enforce count limit.
    let count = ContextAttachmentsRepository::count(pool, &meeting_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    if count >= MAX_ATTACHMENTS_PER_MEETING {
        return Err(format!(
            "Limite de {} anexos atingido para esta reuni\u{00e3}o",
            MAX_ATTACHMENTS_PER_MEETING
        ));
    }

    // Read + validate the source file.
    let source = PathBuf::from(&source_path);
    let original_name = source
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let read =
        storage::read_text_capped(&source).map_err(|e| format!("{}", e))?;

    // Enforce total-size cap using the truncated (stored) size.
    let truncated_size = read.content.len() as i64;
    let total = ContextAttachmentsRepository::total_size(pool, &meeting_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    if total + truncated_size > MAX_TOTAL_BYTES {
        return Err("Limite total de 1 MB de anexos atingido".to_string());
    }

    let meeting_folder = resolve_meeting_folder(pool, &meeting_id).await?;
    let stored_filename = storage::build_stored_filename(&original_name);

    // Write to disk first.
    let _dest =
        storage::write_attachment(&meeting_folder, &stored_filename, &read.content)
            .map_err(|e| format!("Failed to write attachment: {}", e))?;

    let row = ContextAttachmentRow {
        id: Uuid::new_v4().to_string(),
        meeting_id: meeting_id.clone(),
        original_path: source_path,
        stored_filename: stored_filename.clone(),
        display_name: original_name,
        size_bytes: truncated_size,
        truncated: read.truncated,
        sort_order: ContextAttachmentsRepository::next_sort_order(pool, &meeting_id)
            .await
            .map_err(|e| format!("DB error: {}", e))?,
        created_at: Utc::now().to_rfc3339(),
    };

    // Insert DB row. If it fails, roll back the file write.
    if let Err(e) = ContextAttachmentsRepository::insert(pool, &row).await {
        let _ = storage::delete_attachment(&meeting_folder, &stored_filename);
        return Err(format!("Failed to record attachment: {}", e));
    }

    log_info!(
        "Added attachment {} to meeting {} ({} bytes, truncated={})",
        row.display_name,
        meeting_id,
        row.size_bytes,
        row.truncated
    );

    Ok(ContextAttachmentInfo::from(&row))
}

#[tauri::command]
pub async fn api_remove_context_attachment(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    attachment_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let row = ContextAttachmentsRepository::get(pool, &meeting_id, &attachment_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?
        .ok_or_else(|| "Attachment not found".to_string())?;

    let meeting_folder = resolve_meeting_folder(pool, &meeting_id).await?;
    // Best-effort delete; do not fail if file is already gone.
    if let Err(e) = storage::delete_attachment(&meeting_folder, &row.stored_filename) {
        log_warn!(
            "Failed to delete attachment file {}: {}",
            row.stored_filename,
            e
        );
    }

    ContextAttachmentsRepository::delete(pool, &meeting_id, &attachment_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn api_open_context_attachment<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    attachment_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    let row = ContextAttachmentsRepository::get(pool, &meeting_id, &attachment_id)
        .await
        .map_err(|e| format!("DB error: {}", e))?
        .ok_or_else(|| "Attachment not found".to_string())?;

    let meeting_folder = resolve_meeting_folder(pool, &meeting_id).await?;
    let stored = meeting_folder.join("attachments").join(&row.stored_filename);
    if !stored.exists() {
        return Err("Arquivo n\u{00e3}o encontrado. Remova e re-anexe.".to_string());
    }

    let path_str = stored.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path_str)
            .spawn()
            .map_err(|e| format!("Failed to open attachment: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &path_str])
            .spawn()
            .map_err(|e| format!("Failed to open attachment: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path_str)
            .spawn()
            .map_err(|e| format!("Failed to open attachment: {}", e))?;
    }

    Ok(())
}
