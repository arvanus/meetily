use crate::cli::args::RecordArgs;
use crate::database::manager::DatabaseManager;
use crate::state::AppState;
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// Constrói um app Tauri SEM janela. Reusa os mesmos State do app para que os
/// comandos de gravação não entrem em pânico ao buscar `tauri::State`.
pub fn build_headless_app() -> tauri::Result<tauri::App> {
    use crate::{audio, notifications, summary, whisper_engine};
    use std::sync::Arc;
    use tokio::sync::RwLock;
    type NotifState = Arc<RwLock<Option<notifications::manager::NotificationManager<tauri::Wry>>>>;

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(whisper_engine::parallel_commands::ParallelProcessorState::new())
        .manage(Arc::new(RwLock::new(
            None::<notifications::manager::NotificationManager<tauri::Wry>>,
        )) as NotifState)
        .manage(audio::init_system_audio_state())
        .manage(summary::summary_engine::ModelManagerState(Arc::new(
            tokio::sync::Mutex::new(None),
        )))
        .build(tauri::generate_context!())
}

/// Inicializa o banco no mesmo caminho do app (app_data_dir → meeting_minutes.sqlite).
pub async fn init_database(app: &tauri::AppHandle) -> Result<(), String> {
    let db_manager = DatabaseManager::new_from_app_handle(app)
        .await
        .map_err(|e| format!("Falha ao abrir o banco: {}", e))?;
    app.manage(AppState { db_manager });
    Ok(())
}

/// Lista os dispositivos de áudio disponíveis, agrupados em microfones (input)
/// e saída/loopback de sistema (output).
pub async fn run_list_devices() -> Result<(), String> {
    use crate::audio::devices::configuration::DeviceType;

    let devices = crate::audio::devices::discovery::list_audio_devices()
        .await
        .map_err(|e| format!("Falha ao listar dispositivos: {}", e))?;

    let mics: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Input)
        .collect();
    let system: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Output)
        .collect();

    println!("Microfones:");
    if mics.is_empty() {
        println!("  (nenhum)");
    } else {
        for d in mics {
            println!("  - {}", d.name);
        }
    }

    println!("Sistema (loopback):");
    if system.is_empty() {
        println!("  (nenhum)");
    } else {
        for d in system {
            println!("  - {}", d.name);
        }
    }

    Ok(())
}

/// Lista os modelos de transcrição disponíveis (Whisper e Parakeet) e marca o
/// modelo padrão configurado no banco.
pub async fn run_list_models(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::whisper_engine::ModelStatus as WhisperStatus;
    use crate::parakeet_engine::ModelStatus as ParakeetStatus;

    // 1. Padrão configurado (provider/model) a partir do banco.
    let config = crate::api::api::api_get_transcript_config(
        app.clone(),
        app.state::<AppState>(),
        None,
    )
    .await?;

    let (raw_provider, default_model) = match config {
        Some(c) => (c.provider, c.model),
        None => (String::new(), String::new()),
    };

    // O front grava "localWhisper" para o engine Whisper; normaliza para "whisper".
    let default_provider = match raw_provider.as_str() {
        "localWhisper" => "whisper".to_string(),
        other => other.to_string(),
    };

    if default_provider.is_empty() {
        println!("Padrão: (não configurado)");
    } else {
        println!("Padrão: {}/{}", default_provider, default_model);
    }

    // O engine Parakeet não tem fallback standalone; inicializa para descobrir modelos.
    let _ = crate::parakeet_engine::commands::parakeet_init().await;

    // 2. Whisper
    println!("Whisper:");
    match crate::whisper_engine::commands::whisper_get_available_models().await {
        Ok(models) if !models.is_empty() => {
            for m in models {
                let downloaded = matches!(m.status, WhisperStatus::Available);
                let is_default = default_provider == "whisper" && default_model == m.name;
                println!(
                    "  - {} ({} MB){}{}",
                    m.name,
                    m.size_mb,
                    if downloaded { " [baixado]" } else { "" },
                    if is_default { " (padrão)" } else { "" },
                );
            }
        }
        Ok(_) => println!("  (nenhum)"),
        Err(e) => println!("  (erro: {})", e),
    }

    // 3. Parakeet
    println!("Parakeet:");
    match crate::parakeet_engine::commands::parakeet_get_available_models().await {
        Ok(models) if !models.is_empty() => {
            for m in models {
                let downloaded = matches!(m.status, ParakeetStatus::Available);
                let is_default = default_provider == "parakeet" && default_model == m.name;
                println!(
                    "  - {} ({} MB){}{}",
                    m.name,
                    m.size_mb,
                    if downloaded { " [baixado]" } else { "" },
                    if is_default { " (padrão)" } else { "" },
                );
            }
        }
        Ok(_) => println!("  (nenhum)"),
        Err(e) => println!("  (erro: {})", e),
    }

    Ok(())
}

/// Aguarda Ctrl+C ou, se `duration` for informado, o tempo máximo em segundos.
async fn wait_for_stop(duration: Option<u64>) {
    match duration {
        Some(secs) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {},
            }
        }
        None => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Grava uma reunião no modo NORMAL (com IA): reusa o pipeline do app (mixagem +
/// VAD + transcrição), imprime as linhas finalizadas ao vivo no terminal e, ao
/// parar (Ctrl+C ou --duration), persiste a reunião no banco igual ao app.
pub async fn run_record(app: &tauri::AppHandle, args: RecordArgs) -> Result<(), String> {
    args.validate()?;

    // Nome efetivo controlado pela CLI: usamos para o título da gravação E para a
    // persistência no banco, garantindo que ambos batam.
    let effective_name = match args.name.clone() {
        Some(n) => n,
        None => format!(
            "CLI Meeting {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ),
    };

    // Acumulador de segmentos finalizados (apenas para impressão + salvar no banco).
    // O listener interno do app continua cuidando do transcripts.json em disco.
    let segments: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

    // Registra um listener próprio da CLI no evento transcript-update (somente finais).
    let listener_id = {
        use tauri::Listener;
        let segments = segments.clone();
        let quiet = args.quiet;
        app.listen("transcript-update", move |event: tauri::Event| {
            if let Ok(u) = serde_json::from_str::<
                crate::audio::recording_commands::TranscriptUpdate,
            >(event.payload())
            {
                if !quiet {
                    println!("[{}] {}", u.timestamp, u.text);
                }
                // O formato esperado por api_save_transcript é o
                // crate::api::api::TranscriptSegment (campos obrigatórios:
                // id, text, timestamp; opcionais: audio_start_time, audio_end_time,
                // duration, source). NÃO é o recording_saver::TranscriptSegment.
                let seg = serde_json::json!({
                    "id": format!("seg_{}", u.sequence_id),
                    "text": u.text,
                    "timestamp": u.timestamp,
                    "audio_start_time": u.audio_start_time,
                    "audio_end_time": u.audio_end_time,
                    "duration": u.duration,
                    "source": u.source,
                });
                if let Ok(mut guard) = segments.lock() {
                    guard.push(seg);
                }
            }
        })
    };

    // Inicia a gravação reusando o pipeline do app.
    if let Err(e) = crate::audio::recording_commands::start_recording_with_meeting_name(
        app.clone(),
        Some(effective_name.clone()),
    )
    .await
    {
        // Remove o listener antes de retornar.
        {
            use tauri::Listener;
            app.unlisten(listener_id);
        }
        eprintln!(
            "Modelo não disponível. Baixe pelo app, ou grave sem IA com --record-only."
        );
        return Err(e);
    }

    // Captura o caminho da pasta da reunião AGORA: stop_recording faz take() do
    // RECORDING_MANAGER e não o devolve, então depois do stop a pasta fica indisponível.
    let folder_path = crate::audio::recording_commands::get_meeting_folder_path()
        .await
        .ok()
        .flatten();

    println!("Gravando. Ctrl+C para parar e salvar.");

    // Aguarda Ctrl+C ou o tempo de --duration.
    wait_for_stop(args.duration).await;

    println!("Parando e salvando...");

    // Para a gravação (force-flush + espera workers + grava arquivos finais).
    let stop_args = crate::audio::recording_commands::RecordingArgs {
        save_path: folder_path.clone().unwrap_or_default(),
    };
    if let Err(e) =
        crate::audio::recording_commands::stop_recording(app.clone(), stop_args).await
    {
        use tauri::Listener;
        app.unlisten(listener_id);
        return Err(format!("Falha ao parar a gravação: {}", e));
    }

    // Remove o listener da CLI.
    {
        use tauri::Listener;
        app.unlisten(listener_id);
    }

    // Coleta os segmentos acumulados.
    let segments_vec: Vec<serde_json::Value> = match segments.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => Vec::new(),
    };

    // Persiste no banco EXATAMENTE como o app (cria a reunião e retorna meeting_id).
    let result = crate::api::api::api_save_transcript(
        app.clone(),
        app.state::<AppState>(),
        effective_name.clone(),
        segments_vec,
        folder_path.clone(),
        None,
    )
    .await?;

    let meeting_id = result
        .get("meeting_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let folder_display = folder_path.unwrap_or_else(|| "(sem pasta)".to_string());
    println!("✓ Salvo. meeting_id={} pasta={}", meeting_id, folder_display);

    Ok(())
}
