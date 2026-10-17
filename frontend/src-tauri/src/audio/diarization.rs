// Diarization module - merges speaker labels produced by a diarization run
// (scripts/diarization) into an existing meeting transcript.
//
// The transcript text is kept as is. Each segment receives the speaker whose
// turns overlap it the most, and the label is stored in the `speaker` column.
//
// The diarization script can be run by hand (then imported from a file), or by
// the app itself with a local Python environment that has pyannote.audio.

use crate::audio::ffmpeg::find_ffmpeg_path;
use crate::state::AppState;
use anyhow::{anyhow, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_dialog::DialogExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

/// Speaker label for the microphone (left) channel of a multitrack recording
pub const MIC_SPEAKER: &str = "mic";

/// Speaker label for the system audio (right) channel of a multitrack recording
pub const SYSTEM_SPEAKER: &str = "system";

/// File extensions accepted by the diarization file picker
const DIARIZATION_EXTENSIONS: &[&str] = &["json", "rttm"];

/// A single speaker turn, in seconds from the start of the audio file
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SpeakerTurn {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
    /// "mic" or "system" for multitrack stereo audio, None for mono/mixed audio
    #[serde(default)]
    pub channel: Option<String>,
}

/// diarization.json as written by scripts/diarization/diarize.py
#[derive(Debug, Deserialize)]
struct DiarizationFile {
    turns: Vec<SpeakerTurn>,
}

/// Transcript segment timing loaded from the database
#[derive(Debug, Clone)]
pub struct SegmentTiming {
    pub id: String,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub speaker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationImportResult {
    pub file_path: String,
    pub total_segments: usize,
    pub updated_segments: usize,
    pub speakers: Vec<String>,
}

/// Human readable speaker name (same mapping as the UI, see src/lib/speaker.ts)
pub fn speaker_display_name(speaker: &str) -> String {
    match speaker {
        MIC_SPEAKER => "Me".to_string(),
        SYSTEM_SPEAKER => "Others".to_string(),
        _ => speaker
            .strip_prefix("SPEAKER_")
            .and_then(|n| n.parse::<u32>().ok())
            .map(|n| format!("Speaker {}", n + 1))
            .unwrap_or_else(|| speaker.to_string()),
    }
}

/// Name of the local user, from the OS account (USERNAME on Windows, USER elsewhere).
/// Used to tell the summary LLM who "Me" (the microphone channel) is.
pub fn local_user_name() -> Option<String> {
    let var = if cfg!(windows) { "USERNAME" } else { "USER" };
    std::env::var(var)
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Speaker label for text sent to the summary LLM: like speaker_display_name, but the
/// local user is named ("Me (Lucas)") so the model knows who is speaking
pub fn speaker_summary_label(speaker: &str, user_name: Option<&str>) -> String {
    match (speaker, user_name) {
        (MIC_SPEAKER, Some(name)) => format!("Me ({})", name),
        _ => speaker_display_name(speaker),
    }
}

/// Parse the JSON format written by scripts/diarization/diarize.py
pub fn parse_diarization_json(content: &str) -> Result<Vec<SpeakerTurn>> {
    let file: DiarizationFile = serde_json::from_str(content)
        .map_err(|e| anyhow!("Invalid diarization JSON: {}", e))?;
    Ok(normalize_turns(file.turns))
}

/// Parse an RTTM file (one line per turn):
/// `SPEAKER <file> <channel> <start> <duration> <NA> <NA> <label> <NA> <NA>`
pub fn parse_rttm(content: &str) -> Result<Vec<SpeakerTurn>> {
    let mut turns = Vec::new();

    for (line_no, line) in content.lines().enumerate() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() != Some(&"SPEAKER") {
            continue;
        }
        if fields.len() < 8 {
            return Err(anyhow!("Invalid RTTM line {}: {}", line_no + 1, line));
        }

        let start: f64 = fields[3]
            .parse()
            .map_err(|_| anyhow!("Invalid start time on RTTM line {}", line_no + 1))?;
        let duration: f64 = fields[4]
            .parse()
            .map_err(|_| anyhow!("Invalid duration on RTTM line {}", line_no + 1))?;

        turns.push(SpeakerTurn {
            start,
            end: start + duration,
            speaker: fields[7].to_string(),
            channel: None,
        });
    }

    Ok(normalize_turns(turns))
}

/// Load speaker turns from a .json or .rttm file
pub fn load_turns(path: &Path) -> Result<Vec<SpeakerTurn>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("Failed to read {}: {}", path.display(), e))?;

    let is_rttm = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("rttm"))
        .unwrap_or(false);

    if is_rttm {
        parse_rttm(&content)
    } else {
        parse_diarization_json(&content)
    }
}

/// Drop empty or invalid turns and sort by start time
fn normalize_turns(mut turns: Vec<SpeakerTurn>) -> Vec<SpeakerTurn> {
    turns.retain(|t| {
        t.start.is_finite() && t.end.is_finite() && t.end > t.start && !t.speaker.trim().is_empty()
    });
    turns.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    turns
}

/// Assign a speaker to each segment by total time overlap with the turns.
/// Returns (segment_id, new_speaker) only for segments whose label changes.
///
/// Rules:
/// - Multitrack turns (with channel info): segments already labelled "mic" are kept,
///   since the microphone channel is the local user. Segments labelled "system" only
///   consider turns from the system channel.
/// - Every other segment (no label, or a label from an earlier diarization import)
///   considers all turns.
/// - Overlap is summed per speaker, so a segment spanning several short turns of the
///   same speaker still picks that speaker. Ties go to the first label in sort order.
/// - Segments without timing or without any overlapping turn are left unchanged.
///
/// `turns` must be sorted by start time, as returned by the parsers above.
pub fn assign_speakers(segments: &[SegmentTiming], turns: &[SpeakerTurn]) -> Vec<(String, String)> {
    let multitrack = turns.iter().any(|t| t.channel.is_some());
    let mut updates = Vec::new();

    for segment in segments {
        let (Some(start), Some(end)) = (segment.start, segment.end) else {
            continue;
        };
        if end <= start {
            continue;
        }

        let current = segment.speaker.as_deref();
        if multitrack && current == Some(MIC_SPEAKER) {
            continue;
        }
        let system_only = multitrack && current == Some(SYSTEM_SPEAKER);

        let mut scores: BTreeMap<&str, f64> = BTreeMap::new();
        for turn in turns {
            if turn.start >= end {
                break;
            }
            if system_only && turn.channel.as_deref() == Some(MIC_SPEAKER) {
                continue;
            }
            let overlap = end.min(turn.end) - start.max(turn.start);
            if overlap > 0.0 {
                *scores.entry(turn.speaker.as_str()).or_insert(0.0) += overlap;
            }
        }

        let mut best: Option<(&str, f64)> = None;
        for (speaker, score) in scores {
            if best.map_or(true, |(_, best_score)| score > best_score) {
                best = Some((speaker, score));
            }
        }

        if let Some((speaker, _)) = best {
            if current != Some(speaker) {
                updates.push((segment.id.clone(), speaker.to_string()));
            }
        }
    }

    updates
}

/// Pick a diarization file (.json or .rttm) and merge its speaker labels into the
/// meeting transcripts. Returns None if the user cancels the file dialog.
#[tauri::command]
pub async fn import_diarization_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: Option<String>,
) -> Result<Option<DiarizationImportResult>, String> {
    info!("Opening file dialog for diarization import (meeting {})", meeting_id);

    // Use spawn_blocking to avoid blocking async runtime
    let app_clone = app.clone();
    let file_path = tokio::task::spawn_blocking(move || {
        let mut builder = app_clone
            .dialog()
            .file()
            .add_filter("Diarization Files", DIARIZATION_EXTENSIONS);
        if let Some(folder) = meeting_folder_path.filter(|f| Path::new(f).is_dir()) {
            builder = builder.set_directory(folder);
        }
        builder.blocking_pick_file()
    })
    .await
    .map_err(|e| format!("File dialog task failed: {}", e))?;

    let Some(path) = file_path else {
        info!("User cancelled diarization file selection");
        return Ok(None);
    };
    let path_str = path.to_string();
    info!("User selected diarization file: {}", path_str);

    import_diarization(&app, &meeting_id, &path_str)
        .await
        .map(Some)
        .map_err(|e| {
            warn!("Diarization import failed: {}", e);
            e.to_string()
        })
}

pub(crate) async fn import_diarization<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    file_path: &str,
) -> Result<DiarizationImportResult> {
    let turns = load_turns(Path::new(file_path))?;
    if turns.is_empty() {
        return Err(anyhow!("Diarization file has no speaker turns"));
    }

    let app_state = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("App state not available"))?;
    let pool = app_state.db_manager.pool();

    let rows = sqlx::query_as::<_, (String, Option<f64>, Option<f64>, Option<String>)>(
        "SELECT id, audio_start_time, audio_end_time, speaker FROM transcripts WHERE meeting_id = ?",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| anyhow!("Failed to load transcripts: {}", e))?;

    if rows.is_empty() {
        return Err(anyhow!("Meeting has no transcripts"));
    }

    let segments: Vec<SegmentTiming> = rows
        .into_iter()
        .map(|(id, start, end, speaker)| SegmentTiming { id, start, end, speaker })
        .collect();

    let updates = assign_speakers(&segments, &turns);

    let mut conn = pool.acquire().await.map_err(|e| anyhow!("DB error: {}", e))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(|e| anyhow!("Failed to start transaction: {}", e))?;

    for (id, speaker) in &updates {
        sqlx::query("UPDATE transcripts SET speaker = ? WHERE id = ?")
            .bind(speaker)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow!("Failed to update transcript speaker: {}", e))?;
    }

    tx.commit()
        .await
        .map_err(|e| anyhow!("Failed to commit transaction: {}", e))?;

    // Distinct speakers in the transcript after the merge
    let updated: BTreeMap<&str, &str> = updates
        .iter()
        .map(|(id, speaker)| (id.as_str(), speaker.as_str()))
        .collect();
    let speakers: BTreeSet<String> = segments
        .iter()
        .filter_map(|s| {
            updated
                .get(s.id.as_str())
                .map(|sp| sp.to_string())
                .or_else(|| s.speaker.clone())
        })
        .collect();

    info!(
        "Diarization import for meeting {}: {} of {} segments updated, speakers: {:?}",
        meeting_id,
        updates.len(),
        segments.len(),
        speakers
    );

    Ok(DiarizationImportResult {
        file_path: file_path.to_string(),
        total_segments: segments.len(),
        updated_segments: updates.len(),
        speakers: speakers.into_iter().collect(),
    })
}

// ============================================================================
// Running the diarization script from the app
// ============================================================================

/// Diarization script, embedded so the app can run it without a repository checkout
const DIARIZE_SCRIPT: &str = include_str!("../../../../scripts/diarization/diarize.py");

/// Environment variable overriding the Python interpreter used for diarization
const PYTHON_ENV_VAR: &str = "MEETILY_DIARIZE_PYTHON";

/// Output file written by the script inside the meeting folder
const DIARIZATION_OUTPUT: &str = "diarization.json";

/// Number of trailing script output lines kept for error messages
const ERROR_TAIL_LINES: usize = 6;

/// Global flag to track if diarization is in progress
static DIARIZATION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Cancellation token of the running diarization, if any
static DIARIZATION_CANCEL: Mutex<Option<CancellationToken>> = Mutex::new(None);

/// RAII guard for DIARIZATION_IN_PROGRESS and DIARIZATION_CANCEL
struct DiarizationGuard;

impl DiarizationGuard {
    fn acquire(token: CancellationToken) -> Result<Self, String> {
        if DIARIZATION_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Diarization already in progress".to_string());
        }
        *DIARIZATION_CANCEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
        Ok(DiarizationGuard)
    }
}

impl Drop for DiarizationGuard {
    fn drop(&mut self) {
        *DIARIZATION_CANCEL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        DIARIZATION_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiarizationProgress {
    pub meeting_id: String,
    pub progress_percentage: u32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiarizationComplete {
    pub meeting_id: String,
    pub result: DiarizationImportResult,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiarizationError {
    pub meeting_id: String,
    pub error: String,
}

/// Parse a `PROGRESS <percent> <message>` line printed by diarize.py --progress
pub fn parse_progress_line(line: &str) -> Option<(u32, String)> {
    let rest = line.trim().strip_prefix("PROGRESS ")?;
    let (percent, message) = rest.split_once(' ').unwrap_or((rest, ""));
    let percent: u32 = percent.parse().ok()?;
    Some((percent.min(100), message.trim().to_string()))
}

/// Python interpreter for diarization: MEETILY_DIARIZE_PYTHON, or the venv
/// suggested in scripts/diarization/README.md (~/.venvs/meetily-diarize)
fn find_python() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(PYTHON_ENV_VAR) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "{} points to {}, which does not exist",
            PYTHON_ENV_VAR,
            path.display()
        ));
    }

    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = std::env::var_os(home_var)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{} is not set", home_var))?;
    let venv = home.join(".venvs").join("meetily-diarize");
    let python = if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    };

    if python.is_file() {
        Ok(python)
    } else {
        Err(anyhow!(
            "Python environment for diarization not found at {}. Set it up as described in \
             scripts/diarization/README.md, or set {} to a Python with pyannote.audio installed.",
            venv.display(),
            PYTHON_ENV_VAR
        ))
    }
}

/// Write the embedded script to the app data folder (only when it changed)
fn write_script<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| anyhow!("Failed to resolve app data dir: {}", e))?
        .join("diarization");
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow!("Failed to create {}: {}", dir.display(), e))?;

    let path = dir.join("diarize.py");
    let up_to_date = std::fs::read_to_string(&path)
        .map(|current| current == DIARIZE_SCRIPT)
        .unwrap_or(false);
    if !up_to_date {
        std::fs::write(&path, DIARIZE_SCRIPT)
            .map_err(|e| anyhow!("Failed to write {}: {}", path.display(), e))?;
    }
    Ok(path)
}

fn emit_progress<R: Runtime>(app: &AppHandle<R>, meeting_id: &str, percent: u32, message: &str) {
    let _ = app.emit(
        "diarization-progress",
        DiarizationProgress {
            meeting_id: meeting_id.to_string(),
            progress_percentage: percent,
            message: message.to_string(),
        },
    );
}

/// Run diarize.py on the meeting folder, then import the result.
/// `on_progress` receives (percent, message) updates.
pub(crate) async fn run_diarization<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    folder: &Path,
    num_speakers: Option<u32>,
    max_speakers: Option<u32>,
    cancel: CancellationToken,
    on_progress: &(dyn Fn(u32, &str) + Send + Sync),
) -> Result<DiarizationImportResult> {
    if !folder.is_dir() {
        return Err(anyhow!("Meeting folder not found: {}", folder.display()));
    }

    on_progress(0, "Starting diarization...");
    let python = find_python()?;
    let script = write_script(app)?;
    let output = folder.join(DIARIZATION_OUTPUT);

    let mut command = tokio::process::Command::new(&python);
    command
        .arg("-I")
        .arg("-u")
        .arg(&script)
        .arg(folder)
        .arg("--progress")
        .arg("-o")
        .arg(&output);
    if let Some(n) = num_speakers.filter(|n| *n > 0) {
        command.arg("--num-speakers").arg(n.to_string());
    } else if let Some(n) = max_speakers.filter(|n| *n > 0) {
        command.arg("--max-speakers").arg(n.to_string());
    }
    if let Some(ffmpeg) = find_ffmpeg_path() {
        command.arg("--ffmpeg").arg(ffmpeg);
    }
    command
        .env("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Hide console window on Windows
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    info!("Running diarization for meeting {}: {:?}", meeting_id, command);
    let mut child = command
        .spawn()
        .map_err(|e| anyhow!("Failed to start Python ({}): {}", python.display(), e))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Failed to capture diarization output"))?;

    // Script progress maps to 0-95%, the import takes the rest
    let mut reader = BufReader::new(stderr);
    let mut buffer = Vec::new();
    let mut tail: VecDeque<String> = VecDeque::new();
    loop {
        buffer.clear();
        let read = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                return Err(anyhow!("Diarization cancelled"));
            }
            read = reader.read_until(b'\n', &mut buffer) => read,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                if let Some((percent, message)) = parse_progress_line(&line) {
                    on_progress(percent * 95 / 100, &message);
                } else {
                    let line = line.trim();
                    if !line.is_empty() {
                        debug!("diarize.py: {}", line);
                        if tail.len() == ERROR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line.to_string());
                    }
                }
            }
        }
    }

    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            return Err(anyhow!("Diarization cancelled"));
        }
        status = child.wait() => status.map_err(|e| anyhow!("Failed to wait for diarization: {}", e))?,
    };
    if !status.success() {
        let details = tail.into_iter().collect::<Vec<_>>().join("\n");
        return Err(anyhow!("Diarization failed ({}):\n{}", status, details));
    }

    on_progress(97, "Importing speakers...");
    let output_str = output.to_string_lossy().to_string();
    import_diarization(app, meeting_id, &output_str).await
}

/// Run the diarization script on a meeting and merge the result into its transcript.
/// Progress, completion and errors are reported through events.
#[tauri::command]
pub async fn start_diarization_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    num_speakers: Option<u32>,
    max_speakers: Option<u32>,
) -> Result<(), String> {
    let token = CancellationToken::new();
    let guard = DiarizationGuard::acquire(token.clone())?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let folder = PathBuf::from(&meeting_folder_path);
        let on_progress = |percent: u32, message: &str| {
            emit_progress(&app, &meeting_id, percent, message)
        };
        let result =
            run_diarization(
                &app,
                &meeting_id,
                &folder,
                num_speakers,
                max_speakers,
                token,
                &on_progress,
            )
            .await;
        match result {
            Ok(result) => {
                let _ = app.emit(
                    "diarization-complete",
                    DiarizationComplete { meeting_id, result },
                );
            }
            Err(e) => {
                warn!("Diarization failed for meeting {}: {}", meeting_id, e);
                let _ = app.emit(
                    "diarization-error",
                    DiarizationError { meeting_id, error: e.to_string() },
                );
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn cancel_diarization_command() -> Result<(), String> {
    match DIARIZATION_CANCEL.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(token) => {
            token.cancel();
            Ok(())
        }
        None => Err("No diarization in progress".to_string()),
    }
}

#[tauri::command]
pub async fn is_diarization_in_progress_command() -> bool {
    DIARIZATION_IN_PROGRESS.load(Ordering::SeqCst)
}

/// Name of the local user (OS account), used to label "Me" in summary input
#[tauri::command]
pub async fn get_local_user_name_command() -> Option<String> {
    local_user_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_speaker_summary_label() {
        assert_eq!(speaker_summary_label("mic", Some("Lucas")), "Me (Lucas)");
        assert_eq!(speaker_summary_label("mic", None), "Me");
        assert_eq!(speaker_summary_label("SPEAKER_01", Some("Lucas")), "Speaker 2");
        assert_eq!(speaker_summary_label("system", Some("Lucas")), "Others");
    }

    #[test]
    fn test_speaker_display_name() {
        assert_eq!(speaker_display_name("mic"), "Me");
        assert_eq!(speaker_display_name("system"), "Others");
        assert_eq!(speaker_display_name("SPEAKER_00"), "Speaker 1");
        assert_eq!(speaker_display_name("SPEAKER_12"), "Speaker 13");
        assert_eq!(speaker_display_name("Alice"), "Alice");
    }

    #[test]
    fn test_parse_progress_line() {
        assert_eq!(
            parse_progress_line("PROGRESS 42 System audio: embeddings\n"),
            Some((42, "System audio: embeddings".to_string()))
        );
        assert_eq!(parse_progress_line("PROGRESS 100"), Some((100, String::new())));
        assert_eq!(parse_progress_line("PROGRESS 250 Done"), Some((100, "Done".to_string())));
        assert_eq!(parse_progress_line("PROGRESS abc x"), None);
        assert_eq!(parse_progress_line("Loaded pyannote on cuda"), None);
    }

    fn turn(start: f64, end: f64, speaker: &str, channel: Option<&str>) -> SpeakerTurn {
        SpeakerTurn {
            start,
            end,
            speaker: speaker.to_string(),
            channel: channel.map(|c| c.to_string()),
        }
    }

    fn segment(id: &str, start: f64, end: f64, speaker: Option<&str>) -> SegmentTiming {
        SegmentTiming {
            id: id.to_string(),
            start: Some(start),
            end: Some(end),
            speaker: speaker.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_parse_diarization_json() {
        let json = r#"{
            "version": 1,
            "turns": [
                {"start": 5.0, "end": 7.5, "speaker": "SPEAKER_01", "channel": "system"},
                {"start": 0.5, "end": 3.0, "speaker": "mic", "channel": "mic"},
                {"start": 4.0, "end": 4.0, "speaker": "SPEAKER_00"}
            ]
        }"#;
        let turns = parse_diarization_json(json).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0], turn(0.5, 3.0, "mic", Some("mic")));
        assert_eq!(turns[1], turn(5.0, 7.5, "SPEAKER_01", Some("system")));
    }

    #[test]
    fn test_parse_rttm() {
        let rttm = "SPEAKER audio 1 1.50 2.25 <NA> <NA> SPEAKER_00 <NA> <NA>\n\
                    \n\
                    SPEAKER audio 1 0.00 1.00 <NA> <NA> SPEAKER_01 <NA> <NA>\n";
        let turns = parse_rttm(rttm).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0], turn(0.0, 1.0, "SPEAKER_01", None));
        assert_eq!(turns[1], turn(1.5, 3.75, "SPEAKER_00", None));
    }

    #[test]
    fn test_parse_rttm_invalid_line() {
        assert!(parse_rttm("SPEAKER audio 1 abc 1.0 <NA> <NA> S0 <NA> <NA>").is_err());
        assert!(parse_rttm("SPEAKER audio 1").is_err());
    }

    #[test]
    fn test_assign_mono_uses_max_total_overlap() {
        // Segment 0-10: SPEAKER_00 has 3s + 3s, SPEAKER_01 has 4s
        let turns = vec![
            turn(0.0, 3.0, "SPEAKER_00", None),
            turn(3.0, 7.0, "SPEAKER_01", None),
            turn(7.0, 10.0, "SPEAKER_00", None),
        ];
        let segments = vec![segment("a", 0.0, 10.0, None)];
        assert_eq!(assign_speakers(&segments, &turns), vec![("a".to_string(), "SPEAKER_00".to_string())]);
    }

    #[test]
    fn test_assign_mono_reassigns_existing_labels() {
        let turns = vec![turn(0.0, 5.0, "SPEAKER_01", None)];
        let segments = vec![
            segment("a", 1.0, 2.0, Some("mic")),
            segment("b", 2.0, 3.0, Some("SPEAKER_01")),
        ];
        // Without channel info, even "mic" segments are reassigned; unchanged labels are skipped
        assert_eq!(assign_speakers(&segments, &turns), vec![("a".to_string(), "SPEAKER_01".to_string())]);
    }

    #[test]
    fn test_assign_multitrack_keeps_mic_segments() {
        let turns = vec![
            turn(0.0, 5.0, "mic", Some("mic")),
            turn(0.0, 5.0, "SPEAKER_00", Some("system")),
        ];
        let segments = vec![segment("a", 1.0, 4.0, Some("mic"))];
        assert!(assign_speakers(&segments, &turns).is_empty());
    }

    #[test]
    fn test_assign_multitrack_system_ignores_mic_turns() {
        let turns = vec![
            turn(0.0, 10.0, "mic", Some("mic")),
            turn(4.0, 5.0, "SPEAKER_02", Some("system")),
        ];
        let segments = vec![segment("a", 0.0, 10.0, Some("system"))];
        assert_eq!(assign_speakers(&segments, &turns), vec![("a".to_string(), "SPEAKER_02".to_string())]);
    }

    #[test]
    fn test_assign_multitrack_unlabelled_uses_all_turns() {
        let turns = vec![
            turn(0.0, 6.0, "mic", Some("mic")),
            turn(6.0, 8.0, "SPEAKER_00", Some("system")),
        ];
        let segments = vec![segment("a", 0.0, 8.0, None)];
        assert_eq!(assign_speakers(&segments, &turns), vec![("a".to_string(), "mic".to_string())]);
    }

    #[test]
    fn test_assign_no_overlap_or_timing_is_unchanged() {
        let turns = vec![turn(20.0, 30.0, "SPEAKER_00", None)];
        let segments = vec![
            segment("a", 0.0, 10.0, Some("system")),
            SegmentTiming { id: "b".to_string(), start: None, end: None, speaker: None },
            segment("c", 5.0, 5.0, None),
        ];
        assert!(assign_speakers(&segments, &turns).is_empty());
    }

    #[test]
    fn test_assign_tie_picks_first_label() {
        let turns = vec![
            turn(0.0, 2.0, "SPEAKER_01", None),
            turn(2.0, 4.0, "SPEAKER_00", None),
        ];
        let segments = vec![segment("a", 0.0, 4.0, None)];
        assert_eq!(assign_speakers(&segments, &turns), vec![("a".to_string(), "SPEAKER_00".to_string())]);
    }
}
