// audio/recording_commands.rs
//
// Slim Tauri command layer for recording functionality.
// Delegates to transcription and recording modules for actual implementation.

use anyhow::Result;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::task::JoinHandle;

use super::{
    recording_manager::RecordingStartError,
    parse_audio_device,
    default_input_device,   // Get default microphone
    default_output_device,  // Get default system audio
    AudioDevice,
    DefaultDeviceChange,
    RecordingManager,
};
use super::devices::DeviceType;
use super::recording_state::DeviceType as StreamDeviceType;
use super::stream::AudioStream;
use super::device_monitor::{DeviceEvent, DeviceMonitorType};

// Import transcription modules
use super::transcription::{
    self,
    reset_speech_detected_flag,
};

// Re-export TranscriptUpdate for backward compatibility
pub use super::transcription::TranscriptUpdate;

// ============================================================================
// GLOBAL STATE
// ============================================================================

// Simple recording state tracking
static IS_RECORDING: AtomicBool = AtomicBool::new(false);

/// True for the whole `stop_recording` tail (manager taken -> `recording-stopped`
/// emitted). `IS_RECORDING` must stay true through the tail — the frontend polls
/// it to keep the stop UI up — so the mic-disconnect fallback checks this flag
/// too, otherwise a fallback queued before Stop retries against a taken manager
/// and surfaces a spurious "Microphone fallback failed" toast.
static IS_RECORDING_STOPPING: AtomicBool = AtomicBool::new(false);

/// Recording is live and not being torn down — the only state in which the
/// mic-disconnect fallback should run or report.
fn recording_live() -> bool {
    IS_RECORDING.load(Ordering::SeqCst) && !IS_RECORDING_STOPPING.load(Ordering::SeqCst)
}

/// Same as [`is_recording`] for sync callers (window events). Stays true through the
/// stop tail, while the recording is still being saved.
pub(crate) fn is_recording_active() -> bool {
    IS_RECORDING.load(Ordering::SeqCst)
}

/// Recording is live AND the global manager is still the session `s` belongs to.
/// Used by the mic-disconnect fallback to refuse acting on a *later* recording
/// after a Stop/Start swapped the manager out from under an in-flight task.
///
/// NOTE: this locks `RECORDING_MANAGER`. Never call it while already holding
/// that lock (e.g. inside a `RECORDING_MANAGER.lock()` scope) — the std Mutex
/// is non-reentrant and it would self-deadlock. All current callers invoke it
/// outside any held lock; keep it that way.
fn session_live(s: &Arc<super::RecordingState>) -> bool {
    recording_live()
        && RECORDING_MANAGER
            .lock()
            .unwrap()
            .as_ref()
            .map_or(false, |m| Arc::ptr_eq(m.get_state(), s))
}

/// RAII guard for the stop-tail flag. Sets `IS_RECORDING_STOPPING` true on
/// construction and clears it on Drop — including during unwind — so a panic
/// anywhere in the ~320-line stop tail can't leave the flag stuck true and
/// silently kill the mic-disconnect fallback for every later recording.
///
/// This unwind-clears behaviour depends on `panic = "unwind"` (the default).
/// If a release profile ever sets `panic = "abort"`, Drop won't run on panic
/// and the stuck-flag failure mode returns — add a start-time reset then.
struct StoppingGuard;
impl StoppingGuard {
    fn new() -> Self {
        IS_RECORDING_STOPPING.store(true, Ordering::SeqCst);
        StoppingGuard
    }
}
impl Drop for StoppingGuard {
    fn drop(&mut self) {
        IS_RECORDING_STOPPING.store(false, Ordering::SeqCst);
    }
}

/// Shared start-path finalize. Both start commands MUST call this so a new
/// start path can't silently ship with a per-session flag left unreset (e.g.
/// the mic-recovery budget already exhausted).
fn finalize_recording_start() {
    info!("🔍 Setting IS_RECORDING to true and resetting SPEECH_DETECTED_EMITTED");
    IS_RECORDING.store(true, Ordering::SeqCst);
    MIC_FALLBACK_FAILED_ATTEMPTS.store(0, Ordering::SeqCst); // fresh mic-recovery budget per session
    reset_speech_detected_flag(); // reset speech-detected emit latch for the new session
}

// Global recording manager and transcription task to keep them alive during recording
static RECORDING_MANAGER: Mutex<Option<RecordingManager>> = Mutex::new(None);
static TRANSCRIPTION_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

// Listener ID for proper cleanup - prevents microphone from staying active after recording stops
static TRANSCRIPT_LISTENER_ID: Mutex<Option<tauri::EventId>> = Mutex::new(None);

// Audio level emission task handle for cleanup on stop
static AUDIO_LEVEL_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

// Task that migrates capture to the new system default device, for the roles that
// were not pinned to a specific device in preferences. Cleaned up on stop.
static DEFAULT_DEVICE_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

// Meeting id of the row created when the current recording started. Kept after
// stop so the save that follows can finalize that same row instead of creating a
// duplicate meeting. Overwritten by the next recording.
static CURRENT_RECORDING_MEETING_ID: Mutex<Option<String>> = Mutex::new(None);

// Segments transcribed AFTER the recording manager was taken out of the global
// (stop_recording step 1) and before the transcript listener is removed. The
// listener has nowhere to put them at that point, so they land here and are
// merged back into the manager right before the final transcripts.json write.
// Without this, everything the workers drain during shutdown - exactly the tail
// of the meeting - was missing from the saved transcript.
static SHUTDOWN_SEGMENTS: Mutex<Vec<crate::audio::recording_saver::TranscriptSegment>> =
    Mutex::new(Vec::new());

const TRANSCRIPTION_RUNTIME_START_ERROR_CODE: &str =
    "TRANSCRIPTION_RUNTIME_INITIALIZATION_FAILED";
const TRANSCRIPTION_RUNTIME_USER_MESSAGE: &str = "Speech recognition could not initialize. Restart Meetily. If the problem continues, repair or reinstall the app.";

// ============================================================================
// PUBLIC TYPES
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RecordingArgs {
    pub save_path: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct TranscriptionStatus {
    pub chunks_in_queue: usize,
    pub is_processing: bool,
    pub last_activity_ms: u64,
}

fn map_recording_start_error<R: Runtime>(
    app: &AppHandle<R>,
    error: RecordingStartError,
) -> String {
    crate::tray::update_tray_menu(app);

    match error {
        RecordingStartError::TranscriptionRuntime(source) => {
            error!("Failed to initialize speech recognition: {source:#}");
            let error = RecordingStartError::TranscriptionRuntime(source);
            if let Err(emit_error) = app.emit("transcription-error", serde_json::json!({
                "error": error.to_string(),
                "userMessage": TRANSCRIPTION_RUNTIME_USER_MESSAGE,
                "actionable": false,
                "phase": "startup"
            })) {
                error!("Failed to emit transcription runtime startup error: {emit_error}");
            }
            TRANSCRIPTION_RUNTIME_START_ERROR_CODE.to_string()
        }
        RecordingStartError::Other(error) => format!("Failed to start recording: {error}"),
    }
}

// ============================================================================
// DEVICE RESOLUTION
// ============================================================================

/// Resolve the microphone to record with: requested device (if it actually
/// enumerates) → system default → none (system-audio-only recording).
///
/// The device picker has no "no microphone" option: choosing "Default
/// Microphone" sends `None`, so `None` here means "use the system default",
/// NOT "record without a mic". A specifically-requested mic that isn't in
/// cpal's current enumeration (a stale saved device, or a Continuity
/// "iPhone Microphone" that isn't available right now) is downgraded to the
/// system default — the same `default_input_device()` helper the
/// mid-recording disconnect path uses — so start never hard-fails with
/// "Device not found".
///
/// Emits at most one event per call:
/// - `mic-device-switched` — a specific mic was requested but unavailable,
///   and we fell back to the default (reuses the existing frontend listener).
/// - `mic-unavailable` — no usable mic at all; recording proceeds with
///   system audio only. If system audio is also unavailable, start_streams'
///   own guard reports it.
/// Resolving `None` to the default is the user's actual choice, so it's silent.
///
/// ponytail: sync pre-flight substitution (matches Pro), not catch-and-retry —
/// stream.rs keeps its hard-fail as the last line of defense. cpal calls
/// block briefly either way.
fn resolve_mic_or_default<R: Runtime>(
    app: &AppHandle<R>,
    requested_name: Option<&str>,
) -> Option<Arc<super::AudioDevice>> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let requested_specific = requested_name.is_some();

    if let Some(name) = requested_name {
        // Accepts both the decorated "Name (input)" the UI stores and the plain name
        // printed by `--list-devices`
        let device = device_from_explicit_name(name.to_string(), DeviceType::Input);
        let exists = cpal::default_host()
            .input_devices()
            .map(|mut it| it.any(|d| d.name().map(|n| n == device.name).unwrap_or(false)))
            .unwrap_or(false);
        if exists {
            info!("✅ Using requested microphone: '{}'", device.name);
            return Some(Arc::new(device));
        }
        warn!(
            "⚠️ Requested mic '{}' not enumerated - falling back to system default",
            device.name
        );
    }

    match default_input_device() {
        Ok(device) => {
            info!("✅ Using default microphone: '{}'", device.name);
            if requested_specific {
                // Tell the user their selected mic wasn't available and which
                // mic is actually recording. Reuses the mic-device-switched
                // listener the disconnect path wires up.
                let _ = app.emit(
                    "mic-device-switched",
                    serde_json::json!({ "device_name": device.name }),
                );
            }
            Some(Arc::new(device))
        }
        Err(e) => {
            warn!("❌ No microphone available: {} — recording system audio only", e);
            let _ = app.emit("mic-unavailable", serde_json::json!({}));
            None
        }
    }
}

/// System-audio analog of `resolve_mic_or_default`: `Some(name)` -> that device;
/// `None` (or an empty name; "Default System Audio" in the UI) -> default output.
/// Returns `None` only when no output device exists - system audio is optional,
/// mic-only recording proceeds.
///
/// ponytail: no cpal enumeration check (unlike the mic helper) — Linux system
/// devices are Pulse/ALSA monitor *inputs* tagged Output, so output_devices()
/// would false-negative them. stream.rs still hard-fails on a missing device.
fn resolve_system_or_default(requested_name: Option<&str>) -> Option<Arc<super::AudioDevice>> {
    if let Some(name) = requested_name.filter(|name| !name.trim().is_empty()) {
        // Accepts both the decorated "Name (output)" and the plain `--list-devices` name
        let device = device_from_explicit_name(name.to_string(), DeviceType::Output);
        info!("✅ Using requested system audio: '{}'", device.name);
        return Some(Arc::new(device));
    }

    match default_output_device() {
        Ok(device) => {
            info!("✅ Using default system audio: '{}'", device.name);
            Some(Arc::new(device))
        }
        Err(e) => {
            warn!("⚠️ No system audio available: {} — recording will continue with microphone only", e);
            None
        }
    }
}

/// Wake idle audio hardware before checking microphone callbacks, and finish
/// validation before creating any recording resources.
#[cfg(target_os = "macos")]
async fn prepare_audio_for_recording(
    system_device: Option<&super::AudioDevice>,
) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let wake_name = system_device
        .map(|s| s.name.clone())
        .or_else(|| {
            cpal::default_host()
                .default_output_device()
                .and_then(|d| d.name().ok())
        });
    if let Some(name) = wake_name {
        if let Err(e) = super::recording_manager::wake_audio_connection(&name).await {
            warn!("[AUDIO_WAKE] Wake failed: {} — proceeding anyway", e);
        }
    }

    if let Err(e) = super::devices::verify_microphone_access().await {
        error!("Microphone access verification failed: {}", e);
        return Err(format!("Microphone access required: {}", e));
    }
    Ok(())
}

// ============================================================================
// HELPERS
// ============================================================================

/// Route a finished transcript segment to the recording manager, or park it in
/// SHUTDOWN_SEGMENTS when the manager is already out of the global (shutdown in
/// progress). Parked segments are merged back in `drain_shutdown_segments` before
/// the final transcripts.json write, so the tail of the meeting is never dropped.
fn persist_transcript_segment(segment: crate::audio::recording_saver::TranscriptSegment) {
    if let Ok(manager_guard) = RECORDING_MANAGER.lock() {
        if let Some(manager) = manager_guard.as_ref() {
            manager.add_transcript_segment(segment);
            return;
        }
    }

    if let Ok(mut pending) = SHUTDOWN_SEGMENTS.lock() {
        pending.push(segment);
    }
}

/// Take everything parked by `persist_transcript_segment` during shutdown.
fn drain_shutdown_segments() -> Vec<crate::audio::recording_saver::TranscriptSegment> {
    match SHUTDOWN_SEGMENTS.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => Vec::new(),
    }
}

/// Resolve the active transcription engine/model for persistence in transcripts.json.
///
/// Reads the configured provider and queries the currently loaded model from the
/// matching global engine. Returns a normalized engine id ("whisper"/"parakeet")
/// and the loaded model name when known.
async fn resolve_transcription_info<R: Runtime>(app: &AppHandle<R>) -> (String, Option<String>) {
    let provider = match crate::api::api::api_get_transcript_config(
        app.clone(),
        app.clone().state(),
        None,
    )
    .await
    {
        Ok(Some(cfg)) => cfg.provider,
        _ => "parakeet".to_string(),
    };

    let use_parakeet = provider == "parakeet";

    let model = if use_parakeet {
        let engine = {
            let guard = crate::parakeet_engine::commands::PARAKEET_ENGINE
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.as_ref().cloned()
        };
        match engine {
            Some(e) => e.get_current_model().await,
            None => None,
        }
    } else {
        let engine = {
            let guard = crate::whisper_engine::commands::WHISPER_ENGINE
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            guard.as_ref().cloned()
        };
        match engine {
            Some(e) => e.get_current_model().await,
            None => None,
        }
    };

    (
        if use_parakeet { "parakeet" } else { "whisper" }.to_string(),
        model,
    )
}

/// Create the meeting row for a recording that is starting, and start its heartbeat.
///
/// Deliberately best-effort: a missing `AppState` (a fresh install whose database
/// has not been wired yet) or a transient database error must never abort a
/// recording. When it fails, capture proceeds exactly as it did before this flow
/// existed - only crash recovery is unavailable for that session.
///
/// Returns the meeting id when the row was created.
async fn register_recording_meeting<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
) -> Option<String> {
    use crate::database::repositories::meeting::MeetingsRepository;

    // Scope the state guard so it is not held across an await point.
    let pool = {
        match app.try_state::<crate::state::AppState>() {
            Some(state) => state.db_manager.pool().clone(),
            None => {
                warn!("Database unavailable: this recording will not be crash-recoverable");
                *CURRENT_RECORDING_MEETING_ID.lock().unwrap() = None;
                return None;
            }
        }
    };

    let meeting_id = format!("meeting-{}", uuid::Uuid::new_v4());

    // The meeting folder is created while the manager starts, so it is already
    // known here. It stays None when auto-save is off (transcripts only).
    let folder_path = get_meeting_folder_path().await.ok().flatten();

    if let Err(e) = MeetingsRepository::create_recording_meeting(
        &pool,
        &meeting_id,
        title,
        folder_path.as_deref(),
    )
    .await
    {
        warn!("Failed to create the in-progress meeting row: {}", e);
        *CURRENT_RECORDING_MEETING_ID.lock().unwrap() = None;
        return None;
    }

    *CURRENT_RECORDING_MEETING_ID.lock().unwrap() = Some(meeting_id.clone());
    info!("📌 Recording registered as meeting {}", meeting_id);

    spawn_recording_heartbeat(pool, meeting_id.clone());

    Some(meeting_id)
}

/// Keep `updated_at` fresh while the recording runs.
///
/// Recovery uses the gap since the last heartbeat to tell a crashed recording
/// from one that is still running in another process (typically the CLI while
/// the desktop app is open), so this must outlive nothing but the recording.
fn spawn_recording_heartbeat(pool: sqlx::SqlitePool, meeting_id: String) {
    use crate::database::repositories::meeting::MeetingsRepository;

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        interval.tick().await; // The first tick completes immediately; skip it.

        while IS_RECORDING.load(Ordering::SeqCst) {
            interval.tick().await;
            if !IS_RECORDING.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = MeetingsRepository::touch_recording(&pool, &meeting_id).await {
                warn!("Recording heartbeat failed for {}: {}", meeting_id, e);
            }
        }

        info!("Recording heartbeat ended for {}", meeting_id);
    });
}

/// Meeting id created when the current recording started, if any.
///
/// The frontend reads it right after `recording-started` and hands it back on
/// save, so the pending row is finalized instead of duplicated.
#[tauri::command]
pub async fn get_current_recording_meeting_id() -> Result<Option<String>, String> {
    Ok(CURRENT_RECORDING_MEETING_ID.lock().unwrap().clone())
}

/// Same as [`get_current_recording_meeting_id`], for in-process callers (the CLI).
pub fn current_recording_meeting_id() -> Option<String> {
    CURRENT_RECORDING_MEETING_ID.lock().unwrap().clone()
}

// ============================================================================
// RECORDING COMMANDS
// ============================================================================

// ============================================================================
// AUTOMATIC DEFAULT DEVICE MIGRATION
// ============================================================================

/// Minimum time between two migration attempts of the same role
const DEVICE_SWAP_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);

/// Upper bound on migration attempts of the same role inside a rolling minute
const MAX_DEVICE_SWAPS_PER_MINUTE: usize = 4;

/// Open the new default device and hand the stream to the running recording.
///
/// The recording manager lives behind a sync mutex and opening a device awaits, so
/// this reads what it needs, releases the lock, opens the device, and only then locks
/// again to install the stream. On failure the current stream keeps running.
async fn migrate_to_default_device(change: &DefaultDeviceChange) -> Result<(), String> {
    // The guard is dropped explicitly: holding it across the await below would make
    // this future non-Send and it would not compile inside the spawned task.
    let state = {
        let guard = RECORDING_MANAGER.lock().unwrap();
        let state = guard.as_ref().map(|manager| manager.recording_state());
        drop(guard);
        state
    }
    .ok_or_else(|| "recording manager is no longer active".to_string())?;

    // The mic-disconnect fallback may already have moved capture to this same device
    let current = match change.device_type {
        DeviceMonitorType::Microphone => state.get_microphone_device(),
        DeviceMonitorType::SystemAudio => state.get_system_device(),
    };
    if current.map_or(false, |device| device.name == change.new_name) {
        debug!("'{}' is already the active device, nothing to migrate", change.new_name);
        return Ok(());
    }

    let (device, stream_role) = match change.device_type {
        DeviceMonitorType::Microphone => (
            AudioDevice::new(change.new_name.clone(), DeviceType::Input),
            StreamDeviceType::Microphone,
        ),
        DeviceMonitorType::SystemAudio => (
            AudioDevice::new(change.new_name.clone(), DeviceType::Output),
            StreamDeviceType::System,
        ),
    };
    let device = Arc::new(device);

    let stream = AudioStream::create(device.clone(), state, stream_role, None)
        .await
        .map_err(|e| format!("could not open '{}': {}", device.name, e))?;

    let mut guard = RECORDING_MANAGER.lock().unwrap();
    let manager = guard
        .as_mut()
        .ok_or_else(|| "recording stopped while the new device was opening".to_string())?;
    manager.install_migrated_stream(stream, device, change.device_type.clone());

    Ok(())
}

/// Watch for system default device changes and move capture over to the new device.
///
/// Only reached for roles that were resolved from the system default: a device the
/// user pinned in preferences is never swapped underneath them.
fn spawn_default_device_migration<R: Runtime>(
    app: AppHandle<R>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<DefaultDeviceChange>,
) {
    let handle = tokio::spawn(async move {
        // Each migration costs a short gap in that source, and Bluetooth devices
        // flap while connecting, so migrations are rate limited per role.
        let mut recent_swaps: std::collections::VecDeque<(DeviceMonitorType, std::time::Instant)> =
            std::collections::VecDeque::new();

        while let Some(change) = receiver.recv().await {
            if !IS_RECORDING.load(Ordering::SeqCst) {
                break;
            }

            let now = std::time::Instant::now();
            recent_swaps.retain(|(_, at)| now.duration_since(*at) < std::time::Duration::from_secs(60));

            let last_swap = recent_swaps
                .iter()
                .filter(|(role, _)| *role == change.device_type)
                .map(|(_, at)| *at)
                .max();

            if let Some(last) = last_swap {
                if now.duration_since(last) < DEVICE_SWAP_COOLDOWN {
                    warn!(
                        "Skipping {:?} migration to '{}': previous migration was {:?} ago",
                        change.device_type,
                        change.new_name,
                        now.duration_since(last)
                    );
                    continue;
                }
            }

            let swaps_this_minute = recent_swaps
                .iter()
                .filter(|(role, _)| *role == change.device_type)
                .count();
            if swaps_this_minute >= MAX_DEVICE_SWAPS_PER_MINUTE {
                warn!(
                    "Skipping {:?} migration to '{}': device is flapping ({} migrations in the last minute)",
                    change.device_type, change.new_name, swaps_this_minute
                );
                continue;
            }

            let role = match change.device_type {
                DeviceMonitorType::Microphone => "microphone",
                DeviceMonitorType::SystemAudio => "system",
            };

            // Counted before the attempt, not after: a device that consistently fails
            // to open (permissions, exclusive mode) must be rate limited too, or every
            // debounce window would fire another open attempt for the whole recording.
            recent_swaps.push_back((change.device_type.clone(), now));

            match migrate_to_default_device(&change).await {
                Ok(()) => {
                    info!(
                        "✅ Migrated {} capture to the new system default '{}'",
                        role, change.new_name
                    );

                    let _ = app.emit(
                        "audio-device-migrated",
                        serde_json::json!({
                            "role": role,
                            "from": change.old_name,
                            "to": change.new_name,
                        }),
                    );
                }
                Err(e) => {
                    warn!(
                        "⚠️ Could not migrate {} capture to '{}': {} - staying on '{}'",
                        role, change.new_name, e, change.old_name
                    );
                }
            }
        }

        info!("Default device migration task ended");
    });

    // Abort any task left over from an earlier recording: an orphan still holds its
    // receiver and would swap streams on a manager it was not started for.
    let mut global_task = DEFAULT_DEVICE_TASK.lock().unwrap();
    if let Some(previous) = global_task.replace(handle) {
        previous.abort();
    }
}

/// Build a device from a name given explicitly by a caller.
///
/// Accepts both forms in circulation: the decorated "Name (input)" the UI stores and
/// sends, and the plain name printed by `--list-devices`. The role being resolved
/// wins over whatever suffix the name carries.
fn device_from_explicit_name(name: String, device_type: DeviceType) -> AudioDevice {
    match parse_audio_device(&name) {
        Ok(parsed) => AudioDevice::new(parsed.name, device_type),
        Err(_) => AudioDevice::new(name, device_type),
    }
}

/// Resolve both devices for a start path. Each name is the explicit device or, when
/// none was given, the stored preference; `None` means the system default.
///
/// On macOS the audio hardware is woken and microphone access verified before the
/// microphone is resolved, so a Bluetooth link that was asleep enumerates properly.
async fn resolve_recording_devices<R: Runtime>(
    app: &AppHandle<R>,
    microphone_name: Option<String>,
    system_name: Option<String>,
) -> Result<(Option<Arc<AudioDevice>>, Option<Arc<AudioDevice>>), String> {
    #[cfg(not(target_os = "macos"))]
    let microphone = resolve_mic_or_default(app, microphone_name.as_deref());

    let system = resolve_system_or_default(system_name.as_deref());

    #[cfg(target_os = "macos")]
    prepare_audio_for_recording(system.as_deref()).await?;

    #[cfg(target_os = "macos")]
    let microphone = resolve_mic_or_default(app, microphone_name.as_deref());

    Ok((microphone, system))
}

/// Start recording with default devices
pub async fn start_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    start_recording_with_meeting_name(app, None, None, None).await
}

/// Start recording with optional meeting name and optional device overrides.
///
/// `mic_override` / `system_override` take plain device names as printed by
/// `--list-devices` and win over the stored preferences. A role given here is
/// pinned: it does not follow the system default.
pub async fn start_recording_with_meeting_name<R: Runtime>(
    app: AppHandle<R>,
    meeting_name: Option<String>,
    mic_override: Option<String>,
    system_override: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting recording, meeting: {:?}, mic override: {:?}, system override: {:?}",
        meeting_name, mic_override, system_override
    );

    let engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    if let Err(error) = crate::ensure_onnx_runtime_available() {
        return Err(map_recording_start_error(
            &app,
            RecordingStartError::TranscriptionRuntime(error),
        ));
    }

    // Validate that transcription models are available before starting recording
    info!("🔍 Validating transcription model availability before starting recording...");
    if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await {
        error!("Model validation failed: {}", validation_error);

        // Emit error event for frontend - actionable: false to show toast instead of modal
        // (download progress is already shown in top-right toast)
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": validation_error,
            "userMessage": format!("Recording cannot start: {}", validation_error),
            "actionable": false,
            "phase": "startup"
        }));

        return Err(validation_error);
    }
    info!("✅ Transcription model validation passed");

    // Notify frontend that startup has begun (surfaces STARTING state)
    app.emit("recording-starting", serde_json::json!({
        "message": "Recording initialization started"
    })).map_err(|e| e.to_string())?;

    // Load recording preferences to get auto_save, device preferences, AND recording mode
    let (auto_save, preferred_mic_name, preferred_system_name, recording_mode) =
        match super::recording_preferences::load_recording_preferences(&app).await {
            Ok(prefs) => {
                info!("📋 Loaded recording preferences: auto_save={}, recording_mode={:?}, preferred_mic={:?}, preferred_system={:?}",
                      prefs.auto_save, prefs.recording_mode, prefs.preferred_mic_device, prefs.preferred_system_device);
                (prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device, prefs.recording_mode)
            }
            Err(e) => {
                warn!("Failed to load recording preferences, using defaults: {}", e);
                (true, None, None, super::recording_preferences::RecordingMode::Mono)
            }
        };

    // Only a role with neither an explicit device nor a preference is following the
    // system default, and only those migrate automatically when the default changes
    // (headset plugged in, mic unplugged). A pinned device stays pinned, including
    // when it is missing and the resolution below falls back to the default.
    let follow_default_mic = mic_override.is_none() && preferred_mic_name.is_none();
    let follow_default_system = system_override.is_none() && preferred_system_name.is_none();

    // Explicit → Preference → Default. A missing microphone is not fatal: the
    // recording proceeds with system audio only (see resolve_mic_or_default).
    let (microphone_device, system_device) = resolve_recording_devices(
        &app,
        mic_override.or(preferred_mic_name),
        system_override.or(preferred_system_name),
    )
    .await?;

    // Async-first approach - no more blocking operations!
    info!("🚀 Starting async recording initialization");

    // Create new recording manager only after startup validation succeeds
    let mut manager = RecordingManager::new();

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        // Example: Meeting 2025-10-03_08-25-23
        let now = chrono::Local::now();
        format!(
            "Meeting {}",
            now.format("%Y-%m-%d_%H-%M-%S")
        )
    });
    manager.set_meeting_name(Some(effective_meeting_name.clone()));

    // Record which engine/model is producing the transcription (for transcripts.json).
    // Model is already loaded at this point (validated above).
    let (tx_engine, tx_model) = resolve_transcription_info(&app).await;
    manager.set_transcription_info(tx_engine, tx_model);

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    manager.set_follow_system_default(follow_default_mic, follow_default_system);

    // Start recording with resolved devices (replaces start_recording_with_defaults_and_auto_save call)
    let transcription_receiver = manager
        .start_recording(microphone_device, system_device, auto_save, recording_mode)
        .await
        .map_err(|error| map_recording_start_error(&app, error))?;

    // Take the device event receiver BEFORE storing manager globally.
    // A background task will process device events (hot-swap) without frontend polling.
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Get recording state before storing manager (for audio level emission)
    let recording_state_arc = manager.recording_state();

    // Taken before the manager moves into the global; Some only when a role follows the default
    let default_change_receiver = manager.take_default_change_receiver();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    // Spawn background device event processor (mic-disconnect fallback).
    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session);
    }

    // Spawn only after the manager is reachable globally - the task locks it to swap streams
    if let Some(receiver) = default_change_receiver {
        spawn_default_device_migration(app.clone(), receiver);
    }

    // Flip recording live + reset per-session flags (speech-detected latch,
    // mic-recovery budget). Shared with the other start paths - see helper.
    finalize_recording_start();
    let _ = drain_shutdown_segments(); // Drop leftovers from a previous session
    drop(engine_lifecycle_guard);

    // Create the meeting row now, not on stop, so a process that dies mid-recording
    // still leaves something discoverable. Best-effort: never blocks the recording.
    register_recording_meeting(&app, &effective_meeting_name).await;

    // Spawn audio level emission task - reads real RMS from pipeline and emits to frontend
    {
        let app_for_levels = app.clone();
        let level_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(50));
            while IS_RECORDING.load(Ordering::SeqCst) {
                interval.tick().await;
                let mic_rms = recording_state_arc.mic_rms();
                let sys_rms = recording_state_arc.system_rms();

                // Equalizador FFT de 3 bandas a partir da janela recente de áudio
                // mixado (mono, 48kHz). A função retorna zeros se a janela for curta.
                let window = recording_state_arc.recent_mix_window();
                let bands = crate::cli::fft::three_bands(&window, 48_000);

                let update = serde_json::json!({
                    "mic_rms": mic_rms,
                    "system_rms": sys_rms,
                    "bands": [bands[0], bands[1], bands[2]],
                });

                if let Err(e) = app_for_levels.emit("audio-levels", &update) {
                    error!("Failed to emit audio levels: {}", e);
                    break;
                }
            }
            info!("Audio level emission task ended");
        });
        let mut global_level_task = AUDIO_LEVEL_TASK.lock().unwrap();
        *global_level_task = Some(level_task);
    }

    // Start optimized parallel transcription task and store handle
    let task_handle = transcription::start_transcription_task(app.clone(), transcription_receiver);
    {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        *global_task = Some(task_handle);
    }

    // CRITICAL: Listen for transcript-update events and save to recording manager
    // This enables transcript history persistence for page reload sync
    // Store listener ID for cleanup during stop_recording to ensure microphone is released
    {
        use tauri::Listener;
        let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
            // Parse the transcript update from the event payload
            if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
                // transcript-update only receives finals (partials use transcript-partial event)
                // No is_partial check needed — guaranteed to be final

                let segment = crate::audio::recording_saver::TranscriptSegment {
                    id: format!("seg_{}", update.sequence_id),
                    text: update.text.clone(),
                    audio_start_time: update.audio_start_time,
                    audio_end_time: update.audio_end_time,
                    duration: update.duration,
                    display_time: update.timestamp.clone(),
                    confidence: update.confidence,
                    sequence_id: update.sequence_id,
                };

                // Save to recording manager (or to the shutdown buffer if the
                // manager was already taken for cleanup)
                persist_transcript_segment(segment);
            }
        });
        let mut global_listener = TRANSCRIPT_LISTENER_ID.lock().unwrap();
        *global_listener = Some(listener_id);
        info!("✅ Transcript-update event listener registered for history persistence");
    }

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started successfully with parallel processing",
        "devices": ["Default Microphone", "Default System Audio"],
        "workers": 3
    })).map_err(|e| e.to_string())?;

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    info!("✅ Recording started successfully with async-first approach");

    Ok(())
}

/// Start recording in RECORD-ONLY mode (no AI / no transcription).
///
/// Mirrors `start_recording_with_meeting_name` for device/preference/mode
/// resolution so device selection matches the normal path, but with these
/// intentional differences (record without AI, re-transcribe later in the app):
///   - DIFF: NO transcription-model validation gate (a missing model must not
///     block record-only).
///   - DIFF: `auto_save` is FORCED to true (the audio file is the source for the
///     later re-transcription), regardless of prefs.
///   - DIFF: `set_transcription_info(None, None)` — no engine/model in transcripts.json.
///   - DIFF: the `transcription_receiver` is dropped (no transcription task, no
///     internal `transcript-update` listener).
///   - KEEP: the audio-levels emit loop (panel needs mic/system RMS + FFT bands).
pub async fn start_recording_only<R: Runtime>(
    app: AppHandle<R>,
    meeting_name: Option<String>,
    mic_override: Option<String>,
    system_override: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting RECORD-ONLY recording (no AI), meeting: {:?}, mic override: {:?}, system override: {:?}",
        meeting_name, mic_override, system_override
    );

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    // DIFF vs normal path: NO transcription-model validation here — record-only
    // never loads AI, so a missing/downloading model must not block recording.
    // The ONNX Runtime check still applies: the pipeline's VAD runs on it.
    if let Err(error) = crate::ensure_onnx_runtime_available() {
        return Err(map_recording_start_error(
            &app,
            RecordingStartError::TranscriptionRuntime(error),
        ));
    }

    info!("🚀 Starting async record-only initialization (no AI)");

    // Create new recording manager
    let mut manager = RecordingManager::new();

    // Load recording preferences to get device preferences AND recording mode.
    // NOTE: prefs.auto_save is intentionally ignored below (forced true).
    let (_prefs_auto_save, preferred_mic_name, preferred_system_name, recording_mode) =
        match super::recording_preferences::load_recording_preferences(&app).await {
            Ok(prefs) => {
                info!("📋 Loaded recording preferences (record-only): recording_mode={:?}, preferred_mic={:?}, preferred_system={:?}",
                      prefs.recording_mode, prefs.preferred_mic_device, prefs.preferred_system_device);
                (prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device, prefs.recording_mode)
            }
            Err(e) => {
                warn!("Failed to load recording preferences, using defaults: {}", e);
                (true, None, None, super::recording_preferences::RecordingMode::Mono)
            }
        };

    // DIFF vs normal path: force auto_save=true (the saved audio IS the source
    // for re-transcription later in the app).
    let auto_save = true;

    // Same rule as the normal path: only a role with neither an explicit device nor a
    // preference follows the system default and migrates when that default changes.
    let follow_default_mic = mic_override.is_none() && preferred_mic_name.is_none();
    let follow_default_system = system_override.is_none() && preferred_system_name.is_none();

    // Same resolution as the normal path: Explicit → Preference → Default
    let (microphone_device, system_device) = resolve_recording_devices(
        &app,
        mic_override.or(preferred_mic_name),
        system_override.or(preferred_system_name),
    )
    .await?;

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        let now = chrono::Local::now();
        format!("Meeting {}", now.format("%Y-%m-%d_%H-%M-%S"))
    });
    manager.set_meeting_name(Some(effective_meeting_name.clone()));

    // DIFF vs normal path: no engine/model — recording without AI.
    // (`engine` is a String, `model` an Option; empty engine + None signals "no AI".)
    manager.set_transcription_info(String::new(), None);

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    manager.set_follow_system_default(follow_default_mic, follow_default_system);

    // Start recording with resolved devices. auto_save is forced true above.
    let transcription_receiver = manager
        .start_recording(microphone_device, system_device, auto_save, recording_mode)
        .await
        .map_err(|e| format!("Failed to start recording: {}", e))?;

    // DIFF vs normal path: no transcription task is started and no
    // transcript-update listener is registered. The receiver is DRAINED (not
    // dropped): dropping it closes the channel and the pipeline's VAD then
    // spams "Failed to send VAD segment: channel closed" on every speech
    // segment. Draining keeps the channel open and discards the segments;
    // the task ends naturally when the pipeline stops and closes the sender.
    tokio::spawn(async move {
        let mut receiver = transcription_receiver;
        while receiver.recv().await.is_some() {}
    });

    // Take the device event receiver BEFORE storing manager globally (mic-disconnect fallback)
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Get recording state before storing manager (for audio level emission)
    let recording_state_arc = manager.recording_state();

    // Taken before the manager moves into the global; Some only when a role follows the default
    let default_change_receiver = manager.take_default_change_receiver();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session);
    }

    // Spawn only after the manager is reachable globally - the task locks it to swap streams
    if let Some(receiver) = default_change_receiver {
        spawn_default_device_migration(app.clone(), receiver);
    }

    // Flip recording live + reset per-session flags (shared with the other start paths)
    finalize_recording_start();
    let _ = drain_shutdown_segments(); // Drop leftovers from a previous session

    // Create the meeting row now, not on stop, so a process that dies mid-recording
    // still leaves something discoverable. Best-effort: never blocks the recording.
    register_recording_meeting(&app, &effective_meeting_name).await;

    // KEEP (verbatim from normal path): audio-levels emit loop — reads real RMS
    // from pipeline and emits to frontend (mic/system RMS + FFT bands for panel).
    {
        let app_for_levels = app.clone();
        let level_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(50));
            while IS_RECORDING.load(Ordering::SeqCst) {
                interval.tick().await;
                let mic_rms = recording_state_arc.mic_rms();
                let sys_rms = recording_state_arc.system_rms();

                // Equalizador FFT de 3 bandas a partir da janela recente de áudio
                // mixado (mono, 48kHz). A função retorna zeros se a janela for curta.
                let window = recording_state_arc.recent_mix_window();
                let bands = crate::cli::fft::three_bands(&window, 48_000);

                let update = serde_json::json!({
                    "mic_rms": mic_rms,
                    "system_rms": sys_rms,
                    "bands": [bands[0], bands[1], bands[2]],
                });

                if let Err(e) = app_for_levels.emit("audio-levels", &update) {
                    error!("Failed to emit audio levels: {}", e);
                    break;
                }
            }
            info!("Audio level emission task ended");
        });
        let mut global_level_task = AUDIO_LEVEL_TASK.lock().unwrap();
        *global_level_task = Some(level_task);
    }

    // DIFF vs normal path: no transcription task, no transcript-update listener.

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started in record-only mode (no AI)",
        "devices": ["Default Microphone", "Default System Audio"],
        "record_only": true
    })).map_err(|e| e.to_string())?;

    // Update tray menu to reflect recording state (may log a harmless error headless)
    crate::tray::update_tray_menu(&app);

    info!("✅ Record-only recording started successfully (no AI)");

    Ok(())
}

/// Start recording with specific devices.
/// A `None` name means "not specified" (the UI sends null for "Default"), not "no
/// device": that role falls back to the stored preference and then the system default.
pub async fn start_recording_with_devices<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
) -> Result<(), String> {
    start_recording_with_devices_and_meeting(app, mic_device_name, system_device_name, None).await
}

/// Start recording with specific devices and optional meeting name.
/// See [`start_recording_with_devices`] for how a `None` device name is resolved.
pub async fn start_recording_with_devices_and_meeting<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting recording with specific devices: mic={:?}, system={:?}, meeting={:?}",
        mic_device_name, system_device_name, meeting_name
    );

    let engine_lifecycle_guard = super::common::acquire_engine_lifecycle_lock().await;

    // Check if already recording
    let current_recording_state = IS_RECORDING.load(Ordering::SeqCst);
    info!("🔍 IS_RECORDING state check: {}", current_recording_state);
    if current_recording_state {
        return Err("Recording already in progress".to_string());
    }

    if let Err(error) = crate::ensure_onnx_runtime_available() {
        return Err(map_recording_start_error(
            &app,
            RecordingStartError::TranscriptionRuntime(error),
        ));
    }

    // Validate that transcription models are available before starting recording
    info!("🔍 Validating transcription model availability before starting recording...");
    if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await {
        error!("Model validation failed: {}", validation_error);

        // Emit error event for frontend - actionable: false to show toast instead of modal
        // (download progress is already shown in top-right toast)
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": validation_error,
            "userMessage": format!("Recording cannot start: {}", validation_error),
            "actionable": false,
            "phase": "startup"
        }));

        return Err(validation_error);
    }
    info!("✅ Transcription model validation passed");

    // Notify frontend that startup has begun (surfaces STARTING state)
    app.emit("recording-starting", serde_json::json!({
        "message": "Recording initialization started"
    })).map_err(|e| e.to_string())?;

    // Async-first approach for custom devices - no more blocking operations!
    info!("🚀 Starting async recording initialization with custom devices");

    // Create new recording manager
    let mut manager = RecordingManager::new();

    // Load recording preferences for auto_save and for the roles left unspecified
    let (auto_save, preferred_mic_name, preferred_system_name) =
        match super::recording_preferences::load_recording_preferences(&app).await {
            Ok(prefs) => {
                info!("📋 Loaded recording preferences: auto_save={}", prefs.auto_save);
                (prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device)
            }
            Err(e) => {
                warn!("Failed to load recording preferences, defaulting to auto_save=true: {}", e);
                (true, None, None) // Default to saving if preferences can't be loaded
            }
        };

    // A role named here is pinned. A role left as None is NOT "no device": the UI sends
    // null for "Default", so it resolves from the preference and then the system
    // default, and only in that case does it follow the default while recording.
    let follow_default_mic = mic_device_name.is_none() && preferred_mic_name.is_none();
    let follow_default_system = system_device_name.is_none() && preferred_system_name.is_none();

    let (mic_device, system_device) = resolve_recording_devices(
        &app,
        mic_device_name.or(preferred_mic_name),
        system_device_name.or(preferred_system_name),
    )
    .await?;

    // Labels for the "recording-started" event: what was actually resolved, which is
    // more useful than the requested name now that None falls back
    let started_mic_label = mic_device
        .as_ref()
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "No Microphone".to_string());
    let started_system_label = system_device
        .as_ref()
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "No System Audio".to_string());

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        let now = chrono::Local::now();
        format!(
            "Meeting {}",
            now.format("%Y-%m-%d_%H-%M-%S")
        )
    });
    manager.set_meeting_name(Some(effective_meeting_name.clone()));

    // Record which engine/model is producing the transcription (for transcripts.json).
    // Model is already loaded at this point (validated above).
    let (tx_engine, tx_model) = resolve_transcription_info(&app).await;
    manager.set_transcription_info(tx_engine, tx_model);

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    manager.set_follow_system_default(follow_default_mic, follow_default_system);

    // Start recording with specified devices and auto_save setting
    let transcription_receiver = manager
        .start_recording(mic_device, system_device, auto_save, super::recording_preferences::RecordingMode::Mono)
        .await
        .map_err(|error| map_recording_start_error(&app, error))?;

    // Take the device event receiver BEFORE storing manager globally.
    // A background task will process device events (hot-swap) without frontend polling.
    let device_event_receiver = manager.take_device_event_receiver();
    let session = manager.get_state().clone();

    // Taken before the manager moves into the global; Some only when a role follows the default
    let default_change_receiver = manager.take_default_change_receiver();

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    // Spawn background device event processor (mic-disconnect fallback).
    if let Some(receiver) = device_event_receiver {
        spawn_device_event_processor(app.clone(), receiver, session);
    }

    // Spawn only after the manager is reachable globally - the task locks it to swap streams
    if let Some(receiver) = default_change_receiver {
        spawn_default_device_migration(app.clone(), receiver);
    }

    // Flip recording live + reset per-session flags (speech-detected latch,
    // mic-recovery budget). Shared with the other start paths - see helper.
    finalize_recording_start();
    let _ = drain_shutdown_segments(); // Drop leftovers from a previous session
    drop(engine_lifecycle_guard);

    // Create the meeting row now, not on stop, so a process that dies mid-recording
    // still leaves something discoverable. Best-effort: never blocks the recording.
    register_recording_meeting(&app, &effective_meeting_name).await;

    // Start optimized parallel transcription task and store handle
    let task_handle = transcription::start_transcription_task(app.clone(), transcription_receiver);
    {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        *global_task = Some(task_handle);
    }

    // CRITICAL: Listen for transcript-update events and save to recording manager
    // This enables transcript history persistence for page reload sync
    // Store listener ID for cleanup during stop_recording to ensure microphone is released
    {
        use tauri::Listener;
        let listener_id = app.listen("transcript-update", move |event: tauri::Event| {
            // Parse the transcript update from the event payload
            if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
                // transcript-update only receives finals (partials use transcript-partial event)
                // No is_partial check needed — guaranteed to be final

                let segment = crate::audio::recording_saver::TranscriptSegment {
                    id: format!("seg_{}", update.sequence_id),
                    text: update.text.clone(),
                    audio_start_time: update.audio_start_time,
                    audio_end_time: update.audio_end_time,
                    duration: update.duration,
                    display_time: update.timestamp.clone(),
                    confidence: update.confidence,
                    sequence_id: update.sequence_id,
                };

                // Save to recording manager (or to the shutdown buffer if the
                // manager was already taken for cleanup)
                persist_transcript_segment(segment);
            }
        });
        let mut global_listener = TRANSCRIPT_LISTENER_ID.lock().unwrap();
        *global_listener = Some(listener_id);
        info!("✅ Transcript-update event listener registered for history persistence");
    }

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started with custom devices and parallel processing",
        "devices": [started_mic_label, started_system_label],
        "workers": 3
    })).map_err(|e| e.to_string())?;

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    info!("✅ Recording started with custom devices using async-first approach");

    Ok(())
}

/// Stop recording with optimized graceful shutdown ensuring NO transcript chunks are lost
pub async fn stop_recording<R: Runtime>(
    app: AppHandle<R>,
    _args: RecordingArgs,
) -> Result<(), String> {
    info!(
        "🛑 Starting optimized recording shutdown - ensuring ALL transcript chunks are preserved"
    );

    // Check if recording is active
    if !IS_RECORDING.load(Ordering::SeqCst) {
        info!("Recording was not active");
        return Ok(());
    }

    // Emit shutdown progress to frontend
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "stopping_audio",
            "message": "Stopping audio capture...",
            "progress": 20
        }),
    );

    // Step 1: Stop audio capture immediately (no more new chunks) with proper error handling
    let manager_for_cleanup = {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        global_manager.take()
    };

    // Mark the stop tail as in progress so a mic-disconnect fallback that was
    // queued before Stop short-circuits instead of retrying against the taken
    // manager. IS_RECORDING itself stays true until the tail completes — the
    // frontend polls it to keep the stop UI up. RAII so a panic in the tail
    // below can't leave the flag stuck true.
    let _stopping_guard = StoppingGuard::new();

    let stop_result = if let Some(mut manager) = manager_for_cleanup {
        // Use FORCE FLUSH to immediately process all accumulated audio - eliminates 30s delay!
        info!("🚀 Using FORCE FLUSH to eliminate pipeline accumulation delays");
        let result = manager.stop_streams_and_force_flush().await;
        // Store manager back for later cleanup
        let manager_for_cleanup = Some(manager);
        (result, manager_for_cleanup)
    } else {
        warn!("No recording manager found to stop");
        (Ok(()), None)
    };

    let (stop_result, manager_for_cleanup) = stop_result;

    match stop_result {
        Ok(_) => {
            info!("✅ Audio streams stopped successfully - no more chunks will be created");
        }
        Err(e) => {
            error!("❌ Failed to stop audio streams: {}", e);
            // Bailing out here leaves the manager out of the global while the
            // listener is still registered, so every segment the workers keep
            // producing would pile up in SHUTDOWN_SEGMENTS forever. Tear the
            // listener down and hand whatever was parked to the manager we hold.
            {
                use tauri::Listener;
                if let Some(listener_id) = TRANSCRIPT_LISTENER_ID.lock().unwrap().take() {
                    app.unlisten(listener_id);
                }
            }
            let tail = drain_shutdown_segments();
            if !tail.is_empty() {
                match manager_for_cleanup.as_ref() {
                    Some(manager) => manager.add_transcript_segments(tail),
                    None => warn!(
                        "⚠️ {} segment(s) parked during shutdown were dropped (no recording manager)",
                        tail.len()
                    ),
                }
            }
            return Err(format!("Failed to stop audio streams: {}", e));
        }
    }

    // NOTE: Transcript listener is kept alive until AFTER workers finish processing.
    // Moving unlisten before worker completion caused the last flushed segments to be lost,
    // because the listener was removed while workers were still transcribing flushed audio.

    // Step 2: Signal transcription workers to finish processing ALL queued chunks
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "processing_transcripts",
            "message": "Processing remaining transcript chunks...",
            "progress": 40
        }),
    );

    // Wait for transcription task with enhanced progress monitoring (NO TIMEOUT - we must process all chunks)
    let transcription_task = {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        global_task.take()
    };

    if let Some(task_handle) = transcription_task {
        info!("⏳ Waiting for ALL transcription chunks to be processed (no timeout - preserving every chunk)");

        // Enhanced progress monitoring during shutdown
        let progress_app = app.clone();
        let progress_task = tokio::spawn(async move {
            let last_update = std::time::Instant::now();

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                // Emit periodic progress updates during shutdown
                let elapsed = last_update.elapsed().as_secs();
                let _ = progress_app.emit(
                    "recording-shutdown-progress",
                    serde_json::json!({
                        "stage": "processing_transcripts",
                        "message": format!("Processing transcripts... ({}s elapsed)", elapsed),
                        "progress": 40,
                        "detailed": true,
                        "elapsed_seconds": elapsed
                    }),
                );
            }
        });

        // Wait up to 10 minutes for transcription completion to prevent indefinite hangs
        match tokio::time::timeout(
            tokio::time::Duration::from_secs(600), // 10 minutes max
            task_handle
        ).await {
            Ok(Ok(())) => {
                info!("✅ ALL transcription chunks processed successfully - no data lost");
            }
            Ok(Err(e)) => {
                warn!("⚠️ Transcription task completed with error: {:?}", e);
                // Continue anyway - the worker may have processed most chunks
            }
            Err(_) => {
                warn!("⏱️ Transcription timeout (10 minutes) reached, continuing shutdown to prevent indefinite hang");
                // Continue shutdown even on timeout - better to lose some chunks than hang forever
            }
        }

        // Stop progress monitoring
        progress_task.abort();
    } else {
        info!("ℹ️ No transcription task found to wait for");
    }

    // Step 2.5: NOW remove transcript listener - all workers have finished
    {
        use tauri::Listener;
        if let Some(listener_id) = TRANSCRIPT_LISTENER_ID.lock().unwrap().take() {
            app.unlisten(listener_id);
            info!("✅ Transcript-update listener removed (after all workers finished)");
        }
    }

    // Step 2.6: Merge back everything transcribed AFTER the manager was taken in
    // step 1. Those are the segments produced while the workers drained the queue
    // (the end of the meeting) and the listener had no manager to write to.
    {
        let tail = drain_shutdown_segments();
        if !tail.is_empty() {
            match manager_for_cleanup.as_ref() {
                Some(manager) => {
                    info!(
                        "🧩 Merging {} segment(s) transcribed during shutdown into the transcript",
                        tail.len()
                    );
                    manager.add_transcript_segments(tail);
                }
                None => warn!(
                    "⚠️ {} segment(s) transcribed during shutdown could not be saved (no recording manager)",
                    tail.len()
                ),
            }
        }
    }

    // Step 3: Now safely unload Whisper model after ALL chunks are processed
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "unloading_model",
            "message": "Unloading speech recognition model...",
            "progress": 70
        }),
    );

    info!("🧠 All transcript chunks processed. Now safely unloading transcription model...");

    // Determine which provider was used and unload the appropriate model (with timeout)
    let config = match tokio::time::timeout(
        tokio::time::Duration::from_secs(30), // 30 seconds max for DB operation
        crate::api::api::api_get_transcript_config(
            app.clone(),
            app.clone().state(),
            None,
        )
    )
    .await
    {
        Ok(Ok(Some(config))) => Some(config.provider),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            warn!("⚠️ Failed to get transcript config: {:?}", e);
            None
        }
        Err(_) => {
            warn!("⏱️ Transcript config timeout (30s), continuing shutdown");
            None
        }
    };

    match config.as_deref() {
        Some("parakeet") => {
            info!("🦜 Unloading Parakeet model...");
            let engine_clone = {
                let engine_guard = crate::parakeet_engine::commands::PARAKEET_ENGINE
                    .lock()
                    .unwrap();
                engine_guard.as_ref().cloned()
            };

            if let Some(engine) = engine_clone {
                let current_model = engine
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                info!("Current Parakeet model before unload: '{}'", current_model);

                if engine.unload_model().await {
                    info!("✅ Parakeet model '{}' unloaded successfully", current_model);
                } else {
                    warn!("⚠️ Failed to unload Parakeet model '{}'", current_model);
                }
            } else {
                warn!("⚠️ No Parakeet engine found to unload model");
            }
        }
        _ => {
            // Default to Whisper
            info!("🎤 Unloading Whisper model...");
            let engine_clone = {
                let engine_guard = crate::whisper_engine::commands::WHISPER_ENGINE
                    .lock()
                    .unwrap();
                engine_guard.as_ref().cloned()
            };

            if let Some(engine) = engine_clone {
                let current_model = engine
                    .get_current_model()
                    .await
                    .unwrap_or_else(|| "unknown".to_string());
                info!("Current Whisper model before unload: '{}'", current_model);

                if engine.unload_model().await {
                    info!("✅ Whisper model '{}' unloaded successfully", current_model);
                } else {
                    warn!("⚠️ Failed to unload Whisper model '{}'", current_model);
                }
            } else {
                // Expected when no Whisper engine was created this session
                // (e.g. record-only mode or Parakeet as the active provider).
                info!("No Whisper engine found to unload model");
            }
        }
    }

    // Step 3.5: Track meeting ended analytics with privacy-safe metadata
    // Extract all data from manager BEFORE any async operations to avoid Send issues
    let analytics_data = if let Some(ref manager) = manager_for_cleanup {
        let state = manager.get_state();
        let stats = state.get_stats();

        Some((
            manager.get_recording_duration(),
            manager.get_active_recording_duration().unwrap_or(0.0),
            manager.get_total_pause_duration(),
            manager.get_transcript_segments().len() as u64,
            state.has_fatal_error(),
            state.get_microphone_device().map(|d| d.name.clone()),
            state.get_system_device().map(|d| d.name.clone()),
            stats.chunks_processed,
        ))
    } else {
        None
    };

    // Now perform async analytics tracking without holding manager reference
    if let Some((total_duration, active_duration, pause_duration, transcript_segments_count, had_fatal_error, mic_device_name, sys_device_name, chunks_processed)) = analytics_data {
        info!("📊 Collecting analytics for meeting end");

        // Helper function to classify device type from device name (privacy-safe)
        fn classify_device_type(device_name: &str) -> &'static str {
            let name_lower = device_name.to_lowercase();
            // Check for Bluetooth keywords
            if name_lower.contains("bluetooth")
                || name_lower.contains("airpods")
                || name_lower.contains("beats")
                || name_lower.contains("headphones")
                || name_lower.contains("bt ")
                || name_lower.contains("wireless") {
                "Bluetooth"
            } else {
                "Wired"
            }
        }

        // Get transcription model info (already loaded above for model unload)
        let transcription_config = match crate::api::api::api_get_transcript_config(
            app.clone(),
            app.clone().state(),
            None,
        )
        .await
        {
            Ok(Some(config)) => Some((config.provider, config.model)),
            _ => None,
        };

        let (transcription_provider, transcription_model) = transcription_config
            .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

        // Get summary model info from API
        let summary_config = match crate::api::api::api_get_model_config(
            app.clone(),
            app.clone().state(),
            None,
        )
        .await
        {
            Ok(Some(config)) => Some((config.provider, config.model)),
            _ => None,
        };

        let (summary_provider, summary_model) = summary_config
            .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));

        // Classify device types (privacy-safe)
        let microphone_device_type = mic_device_name
            .as_ref()
            .map(|name| classify_device_type(name))
            .unwrap_or("Unknown");

        let system_audio_device_type = sys_device_name
            .as_ref()
            .map(|name| classify_device_type(name))
            .unwrap_or("Unknown");

        // Track meeting ended event with privacy-safe data
        match crate::analytics::commands::track_meeting_ended(
            transcription_provider.clone(),
            transcription_model.clone(),
            summary_provider.clone(),
            summary_model.clone(),
            total_duration,
            active_duration,
            pause_duration,
            microphone_device_type.to_string(),
            system_audio_device_type.to_string(),
            chunks_processed,
            transcript_segments_count,
            had_fatal_error,
        )
        .await
        {
            Ok(_) => info!("✅ Analytics tracked successfully for meeting end"),
            // Non-essential: the analytics client is only initialized by the UI
            // app (and may be disabled by consent), so absence is expected headless.
            Err(e) => info!("Failed to track analytics: {}", e),
        }
    }

    // Step 4: Finalize recording state and cleanup resources safely
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "finalizing",
            "message": "Finalizing recording and cleaning up resources...",
            "progress": 90
        }),
    );

    // Perform final cleanup with the manager if available
    let (meeting_folder, meeting_name) = if let Some(mut manager) = manager_for_cleanup {
        info!("🧹 Performing final cleanup and saving recording data");

        // Extract meeting info BEFORE async operations
        let meeting_folder = manager.get_meeting_folder();
        let meeting_name = manager.get_meeting_name();

        match tokio::time::timeout(
            tokio::time::Duration::from_secs(300), // 5 minutes max for file I/O
            manager.save_recording_only(&app)
        ).await {
            Ok(Ok(_)) => {
                info!("✅ Recording data saved successfully during cleanup");
            }
            Ok(Err(e)) => {
                warn!(
                    "⚠️ Error during recording cleanup (transcripts preserved): {}",
                    e
                );
                // Don't fail shutdown - transcripts are already preserved
            }
            Err(_) => {
                warn!("⏱️ File I/O timeout (5 minutes) reached during save, continuing shutdown");
                // Don't fail shutdown - transcripts are already preserved
            }
        }

        (meeting_folder, meeting_name)
    } else {
        info!("ℹ️ No recording manager available for cleanup");
        (None, None)
    };

    // Set recording flag to false (this also stops the audio level emission task)
    info!("🔍 Setting IS_RECORDING to false");
    IS_RECORDING.store(false, Ordering::SeqCst);
    // IS_RECORDING_STOPPING is cleared by _stopping_guard on scope exit.

    // Clean up audio level emission task
    {
        let mut level_task = AUDIO_LEVEL_TASK.lock().unwrap();
        if let Some(handle) = level_task.take() {
            handle.abort();
        }
    }

    // Clean up default device migration task
    {
        let mut migration_task = DEFAULT_DEVICE_TASK.lock().unwrap();
        if let Some(handle) = migration_task.take() {
            handle.abort();
        }
    }

    // Step 4.5: Prepare metadata for frontend (NO database save)
    // NOTE: We do NOT save to database here. The frontend will save after all transcripts are displayed.
    // This ensures the user sees all transcripts streaming in before the database save happens.
    let (folder_path_str, meeting_name_str) = match (&meeting_folder, &meeting_name) {
        (Some(path), Some(name)) => (
            Some(path.to_string_lossy().to_string()),
            Some(name.clone()),
        ),
        _ => (None, None),
    };

    info!("📤 Preparing recording metadata for frontend save");
    info!("   folder_path: {:?}", folder_path_str);
    info!("   meeting_name: {:?}", meeting_name_str);

    // Database save removed - frontend will handle this after receiving all transcripts
    info!("ℹ️ Skipping database save in Rust - frontend will save after all transcripts received");

    // Step 5: Complete shutdown
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({
            "stage": "complete",
            "message": "Recording stopped successfully",
            "progress": 100
        }),
    );

    // Emit final stop event with folder_path and meeting_name for frontend to save
    app.emit(
        "recording-stopped",
        serde_json::json!({
            "message": "Recording stopped - frontend will save after all transcripts received",
            "folder_path": folder_path_str,
            "meeting_name": meeting_name_str
        }),
    )
    .map_err(|e| e.to_string())?;

    // Update tray menu to reflect stopped state
    crate::tray::update_tray_menu(&app);

    info!("🎉 Recording stopped successfully with ZERO transcript chunks lost");
    Ok(())
}

/// Check if recording is active
pub async fn is_recording() -> bool {
    IS_RECORDING.load(Ordering::SeqCst)
}

/// Get recording statistics
pub async fn get_transcription_status() -> TranscriptionStatus {
    TranscriptionStatus {
        chunks_in_queue: 0,
        is_processing: IS_RECORDING.load(Ordering::SeqCst),
        last_activity_ms: 0,
    }
}

/// Pause the current recording
#[tauri::command]
pub async fn pause_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Pausing recording");

    // Check if currently recording
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and pause it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.pause_recording().map_err(|e| e.to_string())?;

        // Emit pause event to frontend
        app.emit(
            "recording-paused",
            serde_json::json!({
                "message": "Recording paused"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect paused state
        crate::tray::update_tray_menu(&app);

        info!("Recording paused successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Resume the current recording
#[tauri::command]
pub async fn resume_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Resuming recording");

    // Check if currently recording
    if !IS_RECORDING.load(Ordering::SeqCst) {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and resume it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.resume_recording().map_err(|e| e.to_string())?;

        // Emit resume event to frontend
        app.emit(
            "recording-resumed",
            serde_json::json!({
                "message": "Recording resumed"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect resumed state
        crate::tray::update_tray_menu(&app);

        info!("Recording resumed successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Check if recording is currently paused
#[tauri::command]
pub async fn is_recording_paused() -> bool {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.is_paused()
    } else {
        false
    }
}

/// Get detailed recording state
#[tauri::command]
pub async fn get_recording_state() -> serde_json::Value {
    let is_recording = IS_RECORDING.load(Ordering::SeqCst);
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        serde_json::json!({
            "is_recording": is_recording,
            "is_paused": manager.is_paused(),
            "is_active": manager.is_active(),
            "recording_duration": manager.get_recording_duration(),
            "active_duration": manager.get_active_recording_duration(),
            "total_pause_duration": manager.get_total_pause_duration(),
            "current_pause_duration": manager.get_current_pause_duration()
        })
    } else {
        serde_json::json!({
            "is_recording": is_recording,
            "is_paused": false,
            "is_active": false,
            "recording_duration": null,
            "active_duration": null,
            "total_pause_duration": 0.0,
            "current_pause_duration": null
        })
    }
}

/// Get the meeting folder path for the current recording
/// Returns the path if a meeting name was set and folder structure initialized
#[tauri::command]
pub async fn get_meeting_folder_path() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_meeting_folder().map(|p| p.to_string_lossy().to_string()))
    } else {
        Ok(None)
    }
}

/// Get accumulated transcript segments from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_transcript_history() -> Result<Vec<crate::audio::recording_saver::TranscriptSegment>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_transcript_segments())
    } else {
        Ok(Vec::new()) // No recording active, return empty
    }
}

/// Get meeting name from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_recording_meeting_name() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_meeting_name())
    } else {
        Ok(None)
    }
}

// ============================================================================
// DEVICE MONITORING COMMANDS (AirPods/Bluetooth disconnect/reconnect support)
// ============================================================================

/// Get information about the active audio output device
/// Used to warn users about Bluetooth playback issues
#[tauri::command]
pub async fn get_active_audio_output() -> Result<super::playback_monitor::AudioOutputInfo, String> {
    super::playback_monitor::get_active_audio_output()
        .await
        .map_err(|e| format!("Failed to get audio output info: {}", e))
}


// ============================================================================
// MIC HOT-SWAP (disconnect recovery)
// ============================================================================

// Guard against concurrent mic hot-swap tasks. Only used by the disconnect
// fallback path (trigger_mic_fallback_to_default) — the "chase the new
// default" auto-swap has been removed.
static MIC_SWAP_IN_PROGRESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// Bounded retry budget for the disconnect fallback (P1 #2). Counts COMPLETED
// failed attempts; MIC_SWAP_IN_PROGRESS still prevents overlapping swaps.
static MIC_FALLBACK_FAILED_ATTEMPTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const MAX_MIC_FALLBACK_ATTEMPTS: u32 = 3;

/// Perform mic hot-swap using phased locking — never holds RECORDING_MANAGER during I/O
/// except the brief mic-stream stop in Phase 1.
/// If CPAL hangs during stream creation, only this task blocks; stop flow stays unblocked.
async fn perform_mic_hot_swap_task<R: Runtime>(
    new_device_name: String,
    session: &Arc<super::RecordingState>,
    app: AppHandle<R>,
) -> Result<(), String> {
    info!("[HOT_SWAP] Starting mic hot-swap to '{}'", new_device_name);

    match do_mic_swap(&new_device_name, session).await {
        Ok(()) => {
            info!("[HOT_SWAP] Mic switched to '{}'", new_device_name);
            let _ = app.emit("mic-device-switched", serde_json::json!({
                "device_name": new_device_name
            }));
            Ok(())
        }
        Err(e) => {
            if !session_live(session) {
                return Err(e);
            }
            warn!("[HOT_SWAP] First attempt failed: {} — retrying in 500ms", e);
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            match do_mic_swap(&new_device_name, session).await {
                Ok(()) => {
                    info!("[HOT_SWAP] Mic switched to '{}' on retry", new_device_name);
                    let _ = app.emit("mic-device-switched", serde_json::json!({
                        "device_name": new_device_name
                    }));
                    Ok(())
                }
                Err(e) => {
                    error!("[HOT_SWAP] Mic swap failed after retry: {}", e);
                    if session_live(session) {
                        let _ = app.emit("mic-swap-failed", serde_json::json!({
                            "error": e,
                            "device_name": new_device_name
                        }));
                    }
                    Err(e)
                }
            }
        }
    }
}

/// Phased mic swap — lock is never held during async I/O.
async fn do_mic_swap(device_name: &str, session: &Arc<super::RecordingState>) -> Result<(), String> {
    // Phase 1: Lock briefly — verify identity, take old stream OUT (no teardown under lock)
    let old_mic = {
        let mut guard = RECORDING_MANAGER.lock().unwrap();
        let manager = guard.as_mut().ok_or_else(|| "Recording manager not available".to_string())?;
        if !manager.is_recording() {
            return Err("Recording stopped — aborting mic hot-swap".to_string());
        }
        if !Arc::ptr_eq(manager.get_state(), session) {
            return Err("Session changed before hot-swap — aborting".to_string());
        }
        manager.take_mic_stream_for_swap()
    }; // lock released

    // Tear down the dead mic OUTSIDE the lock — cpal stop()/drop on a
    // disconnected BT device can stall on the CoreAudio HAL lock; doing it
    // under RECORDING_MANAGER would freeze stop_recording (deep-review #2).
    // Non-fatal: the replacement stream is created next regardless, so a
    // teardown error/stall on the already-dead device must not abort the swap.
    if let Some(s) = old_mic {
        if let Err(e) = s.stop() {
            warn!("[HOT_SWAP] Failed to stop old mic stream (proceeding): {}", e);
        }
    }

    // Phase 2: Async I/O WITHOUT lock — may be slow, that's OK
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Build the AudioDevice directly from the name — the caller
    // (trigger_mic_fallback_to_default) already resolved it via
    // default_input_device(). Skipping list_audio_devices() here avoids a
    // full cpal enumeration on the exact BT-transition hot path where it's
    // known to hang 100+ s (see H2 in PR-175 review). The real device
    // validation happens inside AudioStream::create → get_device_and_config
    // which does a targeted host.input_devices() lookup by name.
    let device_arc = std::sync::Arc::new(super::AudioDevice::new(
        device_name.to_string(),
        super::DeviceType::Input,
    ));

    info!("[HOT_SWAP] Creating new mic stream for '{}' (lock released)", device_name);
    let new_stream = super::stream::AudioStream::create(
        device_arc.clone(),
        session.clone(),
        super::recording_state::DeviceType::Microphone,
        None,
    ).await.map_err(|e| format!("Failed to create mic stream: {}", e))?;

    // Resolve the current default output OUTSIDE the lock — a CoreAudio stall here
    // must not block stop_recording (which needs RECORDING_MANAGER). (P1 #1)
    let system_name = default_output_device().ok().map(|d| d.name);

    // Phase 3: Lock briefly — install ONLY if still the same session
    {
        let mut guard = RECORDING_MANAGER.lock().unwrap();
        match guard.as_mut() {
            Some(manager) if Arc::ptr_eq(manager.get_state(), session) => {
                manager.set_mic_stream_after_swap(new_stream, device_arc, system_name);
                info!("[HOT_SWAP] Mic hot-swap to '{}' completed", device_name);
            }
            Some(_) => {
                return Err("Session changed during hot-swap — discarding stale mic stream".to_string());
            }
            None => {
                return Err("Recording manager gone during hot-swap".to_string());
            }
        }
    } // lock released

    Ok(())
}

/// Background processor for device monitor events during a recording session.
///
/// The ONLY mid-recording mic switch that is allowed is the fallback from a
/// dead device to the system default, triggered by the device monitor's
/// DeviceDisconnected event. Any other device event is explicitly ignored —
/// recording stays on whatever device was picked at start time until the
/// meeting ends.
///
/// Rationale: auto-swapping to a freshly-connected BT device during recording
/// triggers a reliable hang inside cpal's stream creation on macOS. Locking
/// the device at start eliminates that hang and also makes the recording
/// session predictable.
///
/// The task stops automatically when the receiver is dropped (recording
/// ends / monitor stops).
fn spawn_device_event_processor<R: Runtime>(
    app: AppHandle<R>,
    mut receiver: tokio::sync::mpsc::UnboundedReceiver<DeviceEvent>,
    session: Arc<super::RecordingState>,
) {
    tokio::spawn(async move {
        info!("[DEVICE_EVENTS] Background event processor started");

        while let Some(event) = receiver.recv().await {
            // Skip if recording has stopped
            if !recording_live() {
                info!("[DEVICE_EVENTS] Recording stopped — ignoring event: {:?}", event);
                continue;
            }

            match event {
                DeviceEvent::DeviceDisconnected { ref device_name, ref device_type } => {
                    info!("[DEVICE_EVENTS] Device disconnected: '{}' ({:?})", device_name, device_type);
                    // The only automatic mid-recording mic change allowed:
                    // when the active microphone dies, fall back to the
                    // system default input. Triggered after the device
                    // monitor's polling threshold fires.
                    if matches!(device_type, DeviceMonitorType::Microphone) {
                        let name = device_name.clone();
                        let app_clone = app.clone();
                        let session = session.clone();
                        tokio::spawn(async move {
                            trigger_mic_fallback_to_default(app_clone, name, session).await;
                        });
                    }
                }
                DeviceEvent::DeviceReconnected { ref device_name, ref device_type } => {
                    // Per product decision: once we have fallen back to the
                    // built-in mic we stay there for the rest of the meeting.
                    // This is intentional — just log and do nothing.
                    info!("[DEVICE_EVENTS] Device reconnected: '{}' ({:?}) — staying on current mic (fallback is sticky)", device_name, device_type);
                }
                DeviceEvent::DeviceListChanged => {
                    debug!("[DEVICE_EVENTS] Device list changed");
                }
            }
        }
        info!("[DEVICE_EVENTS] Background event processor stopped (channel closed)");
    });
}

/// Disconnect fallback: swap the active mic to the system default input
/// device. Triggered from the background device event processor after the
/// device monitor's polling threshold (3 × 2s) fires `DeviceDisconnected`
/// for the active microphone.
///
/// `disconnected_name` is the device that just died. We keep it to detect
/// the edge case where macOS hasn't yet updated the system default input
/// away from the dead device — we wait and retry in that case rather than
/// swapping back to the same broken device.
///
/// This function takes the MIC_SWAP_IN_PROGRESS guard itself; the caller
/// must NOT already hold it. If a swap is somehow already running this
/// returns immediately.
async fn trigger_mic_fallback_to_default<R: Runtime>(
    app: AppHandle<R>,
    disconnected_name: String,
    session: Arc<super::RecordingState>,
) {
    if !session_live(&session) {
        info!(
            "[MIC_FALLBACK] Not recording — skipping fallback for '{}'",
            disconnected_name
        );
        return;
    }

    if MIC_FALLBACK_FAILED_ATTEMPTS.load(Ordering::SeqCst) >= MAX_MIC_FALLBACK_ATTEMPTS {
        warn!(
            "[MIC_FALLBACK] {} failed attempts reached — giving up on '{}' (terminal event already announced)",
            MAX_MIC_FALLBACK_ATTEMPTS, disconnected_name
        );
        return;
    }

    if MIC_SWAP_IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        info!(
            "[MIC_FALLBACK] Swap already in progress — skipping fallback for '{}'",
            disconnected_name
        );
        return;
    }

    // Guard that clears MIC_SWAP_IN_PROGRESS on any return path below so a
    // panic or early return can't leave the flag stuck.
    struct SwapGuard;
    impl Drop for SwapGuard {
        fn drop(&mut self) {
            MIC_SWAP_IN_PROGRESS.store(false, Ordering::SeqCst);
        }
    }
    let _guard = SwapGuard;

    info!(
        "[MIC_FALLBACK] Starting fallback from disconnected device '{}'",
        disconnected_name
    );

    // Let macOS finish swapping the system default input away from the dead
    // device. 150ms is enough in practice for the built-in mic to become the
    // default when an explicitly-selected BT device disconnects.
    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;

    // A Stop-A/Start-B during the sleep swapped our session out. Bail silently —
    // emitting or spending the recovery budget here would fire against B with
    // A's device. Covers the default_input_device() error branch below.
    if !session_live(&session) {
        info!("[MIC_FALLBACK] Session no longer live after wait — aborting fallback for '{}'", disconnected_name);
        return;
    }

    // Query the current system default input. If it still reports the
    // disconnected device, back off once more and re-query — this handles
    // the edge case where the OS hasn't propagated the change yet.
    let fallback_name = match default_input_device() {
        Ok(dev) => dev.name,
        Err(e) => {
            error!("[MIC_FALLBACK] Failed to query default input device: {}", e);
            let _ = app.emit(
                "mic-swap-failed",
                serde_json::json!({
                    "error": format!("Failed to query default input: {}", e),
                    "device_name": disconnected_name,
                }),
            );
            let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
            if n == MAX_MIC_FALLBACK_ATTEMPTS {
                let _ = app.emit(
                    "mic-recovery-exhausted",
                    serde_json::json!({ "device_name": disconnected_name }),
                );
            }
            return;
        }
    };

    let fallback_name = if fallback_name == disconnected_name {
        warn!(
            "[MIC_FALLBACK] Default input still reports disconnected device '{}' — retrying after 300ms",
            disconnected_name
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        if !session_live(&session) {
            info!("[MIC_FALLBACK] Session no longer live after retry wait — aborting fallback for '{}'", disconnected_name);
            return;
        }
        match default_input_device() {
            Ok(dev) if dev.name != disconnected_name => dev.name,
            Ok(dev) => {
                error!(
                    "[MIC_FALLBACK] Default input still '{}' after retry — aborting fallback",
                    dev.name
                );
                let _ = app.emit(
                    "mic-swap-failed",
                    serde_json::json!({
                        "error": "System default input still reports disconnected device after retry",
                        "device_name": disconnected_name,
                    }),
                );
                let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
                if n == MAX_MIC_FALLBACK_ATTEMPTS {
                    let _ = app.emit(
                        "mic-recovery-exhausted",
                        serde_json::json!({ "device_name": disconnected_name }),
                    );
                }
                return;
            }
            Err(e) => {
                error!("[MIC_FALLBACK] Failed to re-query default input device: {}", e);
                let _ = app.emit(
                    "mic-swap-failed",
                    serde_json::json!({
                        "error": format!("Failed to re-query default input: {}", e),
                        "device_name": disconnected_name,
                    }),
                );
                let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
                if n == MAX_MIC_FALLBACK_ATTEMPTS {
                    let _ = app.emit(
                        "mic-recovery-exhausted",
                        serde_json::json!({ "device_name": disconnected_name }),
                    );
                }
                return;
            }
        }
    } else {
        fallback_name
    };

    info!(
        "[MIC_FALLBACK] Falling back '{}' → '{}'",
        disconnected_name, fallback_name
    );

    // macOS Core Audio pre-wake for the hot-swap path — before we call the
    // rebuild path (which internally calls `AudioDeviceStart` on the new
    // mic), play 150ms of digital silence through the current system
    // output device to force the Core Audio hardware unit out of its idle
    // power state. Without this, `AudioDeviceStart` can return `noErr` but
    // the IO proc will not fire for 10-30 seconds until some other audio
    // nudges the hardware awake — the "backend idle until you play YouTube"
    // symptom from earlier testing.
    //
    // `wake_audio_connection_for_swap` has a built-in fallback: if the
    // current system device name doesn't enumerate (e.g. the BT output just
    // disappeared), it plays through `default_output_device()` instead,
    // which on macOS will now be the built-in speakers — exactly the
    // hardware unit we want to wake for the fallback mic.
    //
    // Non-fatal: on error we log and proceed to the swap anyway. A failed
    // wake is strictly better than no wake.
    #[cfg(target_os = "macos")]
    {
        // Read from the captured session directly (no manager lock) — this
        // stays correct even if the global manager has since been swapped by
        // a Stop/Start of a different session.
        let sys_device_name = session.get_system_device().map(|d| d.name.clone());
        if let Some(name) = sys_device_name {
            match super::recording_manager::wake_audio_connection_for_swap(&name).await {
                Ok(()) => info!("[MIC_FALLBACK] Pre-swap audio wake completed"),
                Err(e) => warn!(
                    "[MIC_FALLBACK] Pre-swap audio wake failed: {} — proceeding anyway",
                    e
                ),
            }
        } else {
            log::debug!("[MIC_FALLBACK] No system device recorded — skipping pre-swap wake");
        }
    }

    // Stop may have started during the sleeps above — bail before touching
    // the (possibly already taken) manager.
    if !session_live(&session) {
        info!("[MIC_FALLBACK] Recording stopping — aborting fallback for '{}'", disconnected_name);
        return;
    }

    // perform_mic_hot_swap_task performs its own retry-once logic on failure
    // and emits the mic-device-switched / mic-swap-failed events, so we can
    // just delegate here. It does NOT touch MIC_SWAP_IN_PROGRESS internally.
    match perform_mic_hot_swap_task(fallback_name.clone(), &session, app.clone()).await {
        Ok(()) => {
            info!(
                "[MIC_FALLBACK] Fallback complete: now recording via '{}'",
                fallback_name
            );
            MIC_FALLBACK_FAILED_ATTEMPTS.store(0, Ordering::SeqCst);
        }
        Err(e) => {
            error!("[MIC_FALLBACK] Fallback swap failed: {}", e);
            if !session_live(&session) {
                return;
            }
            let n = MIC_FALLBACK_FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
            if n == MAX_MIC_FALLBACK_ATTEMPTS {
                let _ = app.emit(
                    "mic-recovery-exhausted",
                    serde_json::json!({ "device_name": disconnected_name }),
                );
            }
        }
    }
}
