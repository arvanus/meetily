// audio/recovery.rs
//
// Recovery of recordings that were interrupted before they could be finalized.
//
// Every recording (desktop app and CLI alike) inserts its meeting row when it
// STARTS, flagged `status = 'recording'`, and flips it to `'completed'` on a
// clean stop. A row left at `'recording'` is therefore a recording whose process
// died, and this module turns it back into a normal meeting:
//
//   1. the transcript segments are read from `transcripts.json`, which the
//      recording saver rewrites after every finalized segment;
//   2. the audio checkpoints under `.checkpoints/` are merged into `audio.mp4`;
//   3. the row is completed with the recovered segments.
//
// This replaces the browser-storage recovery path for crashes: the CLI runs
// headless and has no webview, so IndexedDB could never see its recordings.

use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Runtime};

use crate::api::TranscriptSegment;
use crate::database::repositories::meeting::MeetingsRepository;
use crate::database::repositories::transcript::TranscriptsRepository;
use crate::state::AppState;

/// A recording that never finished, as offered to the user.
#[derive(Debug, Clone, Serialize)]
pub struct RecoverableMeeting {
    pub id: String,
    pub title: String,
    /// RFC 3339, when the recording started.
    pub created_at: String,
    /// RFC 3339, last heartbeat while it was running.
    pub updated_at: String,
    /// Meeting folder, when the recording was saving to disk.
    pub folder_path: Option<String>,
    /// Segments found in `transcripts.json`.
    pub transcript_count: usize,
    /// Whether audio checkpoints are still on disk.
    pub has_audio: bool,
    /// Heartbeat still fresh: a recording running right now in another process
    /// (typically the CLI while the app is open), not something to recover yet.
    pub is_live: bool,
}

/// A recording whose heartbeat is younger than this is assumed to still be
/// running in another process (usually the CLI while the app is open), not
/// crashed. The heartbeat fires every 30s, so this leaves room for one miss.
const LIVE_HEARTBEAT_SECONDS: i64 = 90;

/// Shape of `transcripts.json` (version 1.1), as written by `common::write_transcripts_json`.
///
/// The segments deserialize straight into the same [`TranscriptSegment`] the
/// writer serialized; the extra `sequence_id` it stores is simply ignored.
#[derive(Debug, Deserialize)]
struct TranscriptsFile {
    #[serde(default)]
    segments: Vec<TranscriptSegment>,
}

/// Read the transcript segments a recording left on disk.
///
/// Returns an empty list rather than an error when the file is missing: a
/// record-only recording never produces one, and its audio is still worth
/// recovering.
fn read_disk_segments(folder: &Path) -> Vec<TranscriptSegment> {
    let path = folder.join("transcripts.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    match serde_json::from_str::<TranscriptsFile>(&raw) {
        Ok(file) => file.segments,
        Err(e) => {
            warn!("Malformed transcripts.json at {}: {}", path.display(), e);
            Vec::new()
        }
    }
}

/// Recordings that never reached a clean stop.
///
/// Rows whose heartbeat is still fresh are flagged `is_live` rather than dropped:
/// they most likely belong to a recording running in another process, but a
/// crash that happened seconds ago looks identical, so the caller re-checks once
/// the heartbeat has had time to go stale instead of hiding it until next boot.
#[tauri::command]
pub async fn api_get_incomplete_meetings<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<RecoverableMeeting>, String> {
    let pool = state.db_manager.pool();

    let meetings = MeetingsRepository::get_incomplete_meetings(pool)
        .await
        .map_err(|e| format!("Failed to list interrupted meetings: {}", e))?;

    let now = chrono::Utc::now();
    let mut recoverable = Vec::new();

    for meeting in meetings {
        let since_heartbeat = (now - meeting.updated_at.0).num_seconds();
        let is_live = since_heartbeat < LIVE_HEARTBEAT_SECONDS;

        let folder = meeting.folder_path.as_ref().map(PathBuf::from);
        let (transcript_count, has_audio) = match folder.as_deref() {
            Some(path) if path.exists() => {
                (read_disk_segments(path).len(), super::incremental_saver::has_checkpoints(path))
            }
            _ => (0, false),
        };

        recoverable.push(RecoverableMeeting {
            id: meeting.id,
            title: meeting.title,
            created_at: meeting.created_at.0.to_rfc3339(),
            updated_at: meeting.updated_at.0.to_rfc3339(),
            folder_path: meeting.folder_path,
            transcript_count,
            has_audio,
            is_live,
        });
    }

    let live = recoverable.iter().filter(|m| m.is_live).count();
    info!(
        "Found {} interrupted meetings ({} still heartbeating)",
        recoverable.len() - live,
        live
    );
    Ok(recoverable)
}

/// Transcript segments of an interrupted meeting, for preview before recovering.
#[tauri::command]
pub async fn api_get_incomplete_meeting_transcripts<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<TranscriptSegment>, String> {
    let pool = state.db_manager.pool();

    let meeting = MeetingsRepository::get_meeting_metadata(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load meeting {}: {}", meeting_id, e))?
        .ok_or_else(|| format!("Meeting {} not found", meeting_id))?;

    match meeting.folder_path {
        Some(folder) => Ok(read_disk_segments(Path::new(&folder))),
        None => Ok(Vec::new()),
    }
}

/// Outcome of recovering one interrupted meeting.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryResult {
    pub meeting_id: String,
    /// Title the meeting was finalized with. Callers that attach extra segments
    /// afterwards must reuse it, or they silently rename the meeting.
    pub title: String,
    pub transcript_count: usize,
    /// "success" | "partial" | "failed" | "none", from the audio merge.
    pub audio_status: String,
    pub audio_file_path: Option<String>,
    pub message: String,
}

/// Recover an interrupted meeting from what its recording left on disk.
///
/// Merges the audio checkpoints, attaches the transcript segments found in
/// `transcripts.json` and completes the meeting. A recording with no transcripts
/// but with audio (record-only, or a crash before the first segment) is still
/// recovered, so it can be re-transcribed from the app later.
#[tauri::command]
pub async fn api_recover_meeting<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<RecoveryResult, String> {
    let pool = state.db_manager.pool();

    let meeting = MeetingsRepository::get_meeting_metadata(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to load meeting {}: {}", meeting_id, e))?
        .ok_or_else(|| format!("Meeting {} not found", meeting_id))?;

    let folder_path = meeting.folder_path.clone();

    // 1. Transcript segments left on disk.
    let segments = match folder_path.as_deref() {
        Some(folder) => read_disk_segments(Path::new(folder)),
        None => Vec::new(),
    };

    // 2. Merge the audio checkpoints back into audio.mp4.
    let mut audio_status = "none".to_string();
    let mut audio_file_path = None;
    let mut audio_message = "No audio checkpoints found".to_string();

    if let Some(ref folder) = folder_path {
        if super::incremental_saver::has_checkpoints(Path::new(folder)) {
            match super::incremental_saver::recover_audio_from_checkpoints(folder.clone(), 48_000)
                .await
            {
                Ok(status) => {
                    audio_status = status.status;
                    audio_file_path = status.audio_file_path;
                    audio_message = status.message;
                }
                Err(e) => {
                    // A failed merge must not block recovering the transcripts.
                    warn!("Audio recovery failed for {}: {}", meeting_id, e);
                    audio_status = "failed".to_string();
                    audio_message = e;
                }
            }
        }
    }

    // 3. Complete the meeting with whatever was recovered.
    let finalized = TranscriptsRepository::finalize_recording_meeting(
        pool,
        &meeting_id,
        &meeting.title,
        &segments,
        folder_path.clone(),
    )
    .await
    .map_err(|e| format!("Failed to finalize meeting {}: {}", meeting_id, e))?;

    if !finalized {
        return Err(format!("Meeting {} disappeared during recovery", meeting_id));
    }

    // 4. Drop the checkpoints only once the merge succeeded, so a failed merge
    //    keeps the raw audio around for a retry.
    if audio_status == "success" {
        if let Some(ref folder) = folder_path {
            if let Err(e) =
                super::incremental_saver::cleanup_checkpoints(folder.clone()).await
            {
                warn!("Checkpoint cleanup failed (non-fatal): {}", e);
            }
        }
    }

    info!(
        "Recovered meeting {} with {} segments (audio: {})",
        meeting_id,
        segments.len(),
        audio_status
    );

    Ok(RecoveryResult {
        meeting_id,
        title: meeting.title,
        transcript_count: segments.len(),
        audio_status,
        audio_file_path,
        message: audio_message,
    })
}

/// Discard an interrupted meeting.
///
/// Removes the database row only. Whatever the recording wrote stays on disk, so
/// a discard is never a data loss the user cannot undo by hand.
#[tauri::command]
pub async fn api_discard_incomplete_meeting<R: Runtime>(
    _app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();

    let _deleted = MeetingsRepository::delete_meeting(pool, &meeting_id)
        .await
        .map_err(|e| format!("Failed to discard meeting {}: {}", meeting_id, e))?;

    info!("Discarded interrupted meeting {}", meeting_id);
    Ok(())
}
