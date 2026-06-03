use crate::database::manager::DatabaseManager;
use crate::state::AppState;
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
