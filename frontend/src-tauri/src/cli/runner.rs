use crate::cli::args::RecordArgs;
use crate::database::manager::DatabaseManager;
use crate::state::AppState;
use std::io::Write;
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
        .map_err(|e| format!("Failed to open the database: {}", e))?;
    app.manage(AppState { db_manager });
    Ok(())
}

/// Lista os dispositivos de áudio disponíveis, agrupados em microfones (input)
/// e saída/loopback de sistema (output).
pub async fn run_list_devices() -> Result<(), String> {
    use crate::audio::devices::configuration::DeviceType;

    let devices = crate::audio::devices::discovery::list_audio_devices()
        .await
        .map_err(|e| format!("Failed to list audio devices: {}", e))?;

    let mics: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Input)
        .collect();
    let system: Vec<_> = devices
        .iter()
        .filter(|d| d.device_type == DeviceType::Output)
        .collect();

    println!("Microphones:");
    if mics.is_empty() {
        println!("  (none)");
    } else {
        for d in mics {
            println!("  - {}", d.name);
        }
    }

    println!("System (loopback):");
    if system.is_empty() {
        println!("  (none)");
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
        println!("Default: (not configured)");
    } else {
        println!("Default: {}/{}", default_provider, default_model);
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
                    if downloaded { " [downloaded]" } else { "" },
                    if is_default { " (default)" } else { "" },
                );
            }
        }
        Ok(_) => println!("  (nenhum)"),
        Err(e) => println!("  (error: {})", e),
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
                    if downloaded { " [downloaded]" } else { "" },
                    if is_default { " (default)" } else { "" },
                );
            }
        }
        Ok(_) => println!("  (nenhum)"),
        Err(e) => println!("  (error: {})", e),
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

    // Idioma da transcrição (--language): repassado direto para a preferência global
    // que o worker/whisper lê a cada chunk. Sem validação — o engine decide o que
    // fazer com o código (ex.: "pt", "en", "auto"; default do app: "auto-translate").
    if let Some(lang) = &args.language {
        if let Err(e) = crate::set_language_preference_internal(lang.clone()) {
            eprintln!("Warning: could not set language preference: {}", e);
        }
    }

    // === OVERRIDE de engine/model (--engine / --model) =========================
    // IMPORTANTE: o worker de transcrição escolhe provider+model SEMPRE a partir da
    // config PERSISTIDA no banco (api_get_transcript_config), inclusive recarregando
    // o modelo para casar com a config mesmo que outro já esteja carregado
    // (vide get_or_init_whisper). Logo, NÃO basta carregar o modelo no engine: a única
    // forma de o override valer é gravar a config. Para não mexer permanentemente nas
    // preferências do usuário, capturamos a config ORIGINAL aqui e a RESTAURAMOS em
    // TODOS os caminhos de saída (sucesso, erro, Ctrl+C) — vide `restore_config!`.
    // Em --record-only não há IA, então o override é ignorado (sem efeito/sem write).
    let override_active = !args.record_only && (args.engine.is_some() || args.model.is_some());

    // Config original (provider, model), conforme o banco a resolve. Usada para
    // restaurar ao final. Capturada SEMPRE que houver override.
    let original_cfg: Option<(String, String)> = if override_active {
        match crate::api::api::api_get_transcript_config(
            app.clone(),
            app.state::<AppState>(),
            None,
        )
        .await
        {
            Ok(Some(c)) => Some((c.provider, c.model)),
            _ => None,
        }
    } else {
        None
    };

    if override_active {
        // Mapeia o --engine amigável (whisper/parakeet) para o provider persistido
        // (localWhisper/parakeet). Se --engine não vier, mantém o provider original.
        let base_provider = original_cfg
            .as_ref()
            .map(|(p, _)| p.clone())
            .unwrap_or_else(|| "localWhisper".to_string());
        let new_provider = match args.engine.as_deref() {
            Some("whisper") | Some("localWhisper") => "localWhisper".to_string(),
            Some("parakeet") => "parakeet".to_string(),
            Some(other) => {
                // Provider desconhecido: avisa e segue com o original (não grava lixo).
                eprintln!(
                    "Warning: unknown --engine '{}'; using the configured provider.",
                    other
                );
                base_provider.clone()
            }
            None => base_provider.clone(),
        };
        // Se --model não vier, mantém o modelo original.
        let new_model = args
            .model
            .clone()
            .or_else(|| original_cfg.as_ref().map(|(_, m)| m.clone()))
            .unwrap_or_default();

        if let Err(e) = crate::api::api::api_save_transcript_config(
            app.clone(),
            app.state::<AppState>(),
            new_provider.clone(),
            new_model.clone(),
            None,
            None,
        )
        .await
        {
            // Falha ao gravar o override: restaura nada (não gravamos) e segue.
            eprintln!("Warning: could not apply the engine/model override: {}", e);
        } else {
            let label_provider = if new_provider == "localWhisper" {
                "whisper"
            } else {
                new_provider.as_str()
            };
            println!(
                "ℹ Temporary override: {}/{} (the original config will be restored when done).",
                label_provider, new_model
            );
        }
    }

    // Helper: restaura a config original (se houver) gravada antes do override.
    // Roda em todos os caminhos de saída. É idempotente/best-effort.
    macro_rules! restore_config {
        () => {
            if let Some((ref p, ref m)) = original_cfg {
                if let Err(e) = crate::api::api::api_save_transcript_config(
                    app.clone(),
                    app.state::<AppState>(),
                    p.clone(),
                    m.clone(),
                    None,
                    None,
                )
                .await
                {
                    eprintln!("Warning: failed to restore the original transcription config: {}", e);
                }
            }
        };
    }
    // ==========================================================================

    // Acumulador de segmentos finalizados (apenas para impressão + salvar no banco).
    // O listener interno do app continua cuidando do transcripts.json em disco.
    let segments: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

    // Rótulo engine/model para o painel: reusa a config padrão do banco (igual à
    // Task 2). Se falhar, mostra "?". Em --record-only não há IA → "sem IA" (7c).
    // Lido APÓS o override acima, para refletir o motor/modelo que será de fato usado.
    let engine_model = if args.record_only {
        "no AI".to_string()
    } else {
        match crate::api::api::api_get_transcript_config(
            app.clone(),
            app.state::<AppState>(),
            None,
        )
        .await
        {
            Ok(Some(c)) => {
                let provider = match c.provider.as_str() {
                    "localWhisper" => "whisper".to_string(),
                    other => other.to_string(),
                };
                format!("{}/{}", provider, c.model)
            }
            _ => "?".to_string(),
        }
    };
    // Rótulo do motor para o banner (7b): "sem IA" em record-only, senão engine_model.
    let engine_label = engine_model.clone();

    // Estado compartilhado do painel vivo (alimentado por audio-levels + contadores).
    let panel = Arc::new(Mutex::new(crate::cli::panel::PanelState {
        engine_model,
        record_only: args.record_only,
        ..Default::default()
    }));

    // Registra um listener próprio da CLI no evento transcript-update (somente finais).
    // Em --record-only NÃO há transcrição, então não registramos este listener.
    let listener_id: Option<tauri::EventId> = if args.record_only {
        None
    } else {
        use tauri::Listener;
        let segments = segments.clone();
        let panel = panel.clone();
        let quiet = args.quiet;
        Some(app.listen("transcript-update", move |event: tauri::Event| {
            if let Ok(u) = serde_json::from_str::<
                crate::audio::recording_commands::TranscriptUpdate,
            >(event.payload())
            {
                // Conta o trecho para o painel.
                if let Ok(mut p) = panel.lock() {
                    p.segments += 1;
                }
                if !quiet {
                    // Limpa a linha de status ANTES de rolar a linha do transcript,
                    // para que painel e texto não se sobreponham; a draw task
                    // repinta o painel no próximo tick.
                    print!("\r\x1b[K[{}] {}\n", u.timestamp, u.text);
                    let _ = std::io::stdout().flush();
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
        }))
    };

    // Listener de níveis de áudio (a cada ~50ms) → alimenta o VU/equalizador do painel.
    let levels_listener_id = {
        use tauri::Listener;
        let panel = panel.clone();
        app.listen("audio-levels", move |e: tauri::Event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
                if let Ok(mut p) = panel.lock() {
                    p.mic_rms = v["mic_rms"].as_f64().unwrap_or(0.0) as f32;
                    p.system_rms = v["system_rms"].as_f64().unwrap_or(0.0) as f32;
                    if let Some(b) = v["bands"].as_array() {
                        for i in 0..3 {
                            p.bands[i] =
                                b.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                        }
                    }
                }
            }
        })
    };

    // (7a) Listener de transcrições PARCIAIS (--partial): o app emite
    // "transcript-partial" com JSON { "text": ... } (worker.rs:229). Sobrescreve a
    // linha atual com o parcial em cinza. Só ativo no modo normal e sem --quiet;
    // as linhas finais (transcript-update) imprimem por cima depois — esperado.
    let partial_listener_id: Option<tauri::EventId> = if args.partial && !args.record_only {
        use tauri::Listener;
        let quiet = args.quiet;
        Some(app.listen("transcript-partial", move |e: tauri::Event| {
            if quiet {
                return;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
                if let Some(t) = v["text"].as_str() {
                    print!("\r\x1b[K\x1b[90m… {}\x1b[0m", t);
                    let _ = std::io::stdout().flush();
                }
            }
        }))
    } else {
        None
    };

    // Helper: remove TODOS os listeners da CLI (transcript-update, audio-levels,
    // transcript-partial). Usado em todos os caminhos de saída.
    macro_rules! unlisten_all {
        () => {{
            use tauri::Listener;
            if let Some(id) = listener_id {
                app.unlisten(id);
            }
            app.unlisten(levels_listener_id);
            if let Some(id) = partial_listener_id {
                app.unlisten(id);
            }
        }};
    }

    // Inicia a gravação reusando o pipeline do app.
    // --record-only: grava SEM IA (não valida/carrega modelo). Caso contrário,
    // caminho normal (com transcrição ao vivo).
    let start_result = if args.record_only {
        crate::audio::recording_commands::start_recording_only(
            app.clone(),
            Some(effective_name.clone()),
        )
        .await
    } else {
        crate::audio::recording_commands::start_recording_with_meeting_name(
            app.clone(),
            Some(effective_name.clone()),
        )
        .await
    };
    if let Err(e) = start_result {
        // Remove os listeners e restaura a config antes de retornar.
        unlisten_all!();
        restore_config!();
        // A dica de modelo só faz sentido no caminho normal (com IA).
        if !args.record_only {
            eprintln!(
                "Transcription model not available. Download it in the app, or record without AI using --record-only."
            );
        }
        return Err(format!("Failed to start recording: {}", e));
    }

    // Captura o caminho da pasta da reunião AGORA: stop_recording faz take() do
    // RECORDING_MANAGER e não o devolve, então depois do stop a pasta fica indisponível.
    let folder_path = crate::audio::recording_commands::get_meeting_folder_path()
        .await
        .ok()
        .flatten();

    // (7b) Banner de início. Rótulos informativos (não fazemos resolução pesada de
    // dispositivo): mic/system vêm de --mic/--system se informados, senão "padrão (SO)".
    let mic_label = args.mic.clone().unwrap_or_else(|| "default (OS)".to_string());
    let sys_label = args.system.clone().unwrap_or_else(|| "default (OS)".to_string());
    let lang_label = args
        .language
        .as_ref()
        .map(|l| format!("   ✓ Language: {}", l))
        .unwrap_or_default();
    println!(
        "✓ Engine: {}   ✓ Mic: {}   ✓ System: {}{}",
        engine_label, mic_label, sys_label, lang_label
    );
    println!(
        "✓ Meeting: \"{}\"  → {}",
        effective_name,
        folder_path.clone().unwrap_or_else(|| "(folder to be created)".into())
    );

    println!("Recording. Press Ctrl+C to stop and save.");

    // Draw task do painel vivo: redesenha ~4x/s na própria linha, sempre (mesmo com
    // --quiet; quiet só esconde o texto dos transcripts). Toggla o ponto a cada ~1s e
    // rastreia silêncio (mic+sis abaixo de 0.01) para o alerta.
    let draw_handle = {
        let panel = panel.clone();
        let record_only = args.record_only;
        let folder_for_bytes = folder_path.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut silent_accum_ms: u64 = 0;
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                interval.tick().await;
                // Em --record-only, o painel mostra "gravado: X MB". Sob auto_save,
                // o áudio é escrito incrementalmente em .checkpoints/ dentro da
                // pasta da reunião, então somamos o tamanho dos arquivos da pasta
                // (best-effort, barato — uma varredura a cada 250ms).
                let bytes = if record_only {
                    folder_for_bytes
                        .as_deref()
                        .map(|p| dir_size_bytes(std::path::Path::new(p)))
                        .unwrap_or(0)
                } else {
                    0
                };
                let line = {
                    let Ok(mut p) = panel.lock() else { continue };
                    let elapsed = started.elapsed().as_secs();
                    p.elapsed_secs = elapsed;
                    p.pulse_on = (elapsed % 2) == 0;
                    if record_only {
                        p.bytes_written = bytes;
                    }
                    // Silêncio: acumula 250ms por tick quando ambos rms < 0.01.
                    if p.mic_rms < 0.01 && p.system_rms < 0.01 {
                        silent_accum_ms = silent_accum_ms.saturating_add(250);
                    } else {
                        silent_accum_ms = 0;
                    }
                    p.silent_secs = silent_accum_ms / 1000;
                    crate::cli::panel::render(&p)
                };
                print!("\r\x1b[K{}", line);
                let _ = std::io::stdout().flush();
            }
        })
    };

    // Aguarda Ctrl+C ou o tempo de --duration.
    wait_for_stop(args.duration).await;

    // Para a draw task e abre uma nova linha para o que vem a seguir.
    draw_handle.abort();
    println!();

    println!("Stopping and saving... (finishing queued transcription; press Ctrl+C again to abandon)");

    // Mostra o progresso do shutdown (o app emite "recording-shutdown-progress"
    // enquanto processa a fila de transcrição) para não parecer congelado quando
    // a fila está grande (ex.: modelo lento que não acompanhou o tempo real).
    let progress_listener = {
        use tauri::Listener;
        app.listen("recording-shutdown-progress", move |e: tauri::Event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
                if let Some(msg) = v["message"].as_str() {
                    print!("\r\x1b[K⏳ {}", msg);
                    let _ = std::io::stdout().flush();
                }
            }
        })
    };

    // Para a gravação (force-flush + espera workers + grava arquivos finais).
    // Um SEGUNDO Ctrl+C aqui significa "desisti de esperar a fila": restauramos a
    // config (override) ANTES de sair — matar o processo sem isso deixava a
    // config do app presa no modelo do override.
    let stop_args = crate::audio::recording_commands::RecordingArgs {
        save_path: folder_path.clone().unwrap_or_default(),
    };
    let stop_result = tokio::select! {
        r = crate::audio::recording_commands::stop_recording(app.clone(), stop_args) => r,
        _ = tokio::signal::ctrl_c() => {
            println!();
            eprintln!(
                "⚠ Aborted while processing the transcription backlog; pending segments were lost \
                 (audio checkpoints remain on disk)."
            );
            restore_config!();
            std::process::exit(130);
        }
    };
    {
        use tauri::Listener;
        app.unlisten(progress_listener);
    }
    print!("\r\x1b[K");
    let _ = std::io::stdout().flush();
    if let Err(e) = stop_result {
        unlisten_all!();
        restore_config!();
        return Err(format!("Failed to stop recording: {}", e));
    }

    // Remove os listeners da CLI e restaura a config original (override).
    unlisten_all!();
    restore_config!();

    // Coleta os segmentos acumulados. Em --record-only sempre é vazio (sem IA);
    // o save mesmo assim CRIA a reunião (vide TranscriptsRepository::save_transcript)
    // com transcripts vazios e a folder_path correta, para o app oferecer Re-transcrever.
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
    .await
    .map_err(|e| format!("Failed to save the meeting to the database: {}", e))?;

    let meeting_id = result
        .get("meeting_id")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let folder_display = folder_path.unwrap_or_else(|| "(no folder)".to_string());
    if args.record_only {
        println!(
            "✓ Recorded (no AI). meeting_id={} folder={}  Re-transcribe in the app whenever you want.",
            meeting_id, folder_display
        );
    } else {
        println!("✓ Saved. meeting_id={} folder={}", meeting_id, folder_display);
    }

    Ok(())
}

/// Soma recursiva (best-effort) do tamanho de todos os arquivos sob `dir`,
/// incluindo subpastas como `.checkpoints/`. Ignora erros silenciosamente
/// (a pasta pode ainda não existir nos primeiros ticks).
fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => total = total.saturating_add(dir_size_bytes(&path)),
            Ok(ft) if ft.is_file() => {
                if let Ok(meta) = entry.metadata() {
                    total = total.saturating_add(meta.len());
                }
            }
            _ => {}
        }
    }
    total
}
