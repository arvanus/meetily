use crate::cli::args::{RecordArgs, RetranscribeArgs, SummarizeArgs};
use crate::database::manager::DatabaseManager;
use crate::state::AppState;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// Comando vindo da task de teclado (raw mode) para o loop async de gravação.
enum KeyCmd {
    /// Tecla 'p': alterna pause/resume.
    TogglePause,
    /// Ctrl+C ou 'q': encerra e salva.
    Stop,
}

/// Habilita o raw mode do terminal e o RESTAURA no Drop (qualquer caminho de saída:
/// fim normal, erro, panic). Em raw mode o Ctrl+C não gera mais o sinal do SO — por
/// isso a task de teclado é quem sinaliza o stop. `enable` devolve None quando não há
/// TTY de entrada (pipe/redirect) ou o raw mode falha: aí o runner cai no fluxo
/// clássico (apenas Ctrl+C/--duration), sem mexer no terminal.
struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Option<Self> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return None;
        }
        match crossterm::terminal::enable_raw_mode() {
            Ok(()) => Some(RawModeGuard),
            Err(_) => None,
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

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
                    // repinta o painel no próximo tick. Usamos \r\n (não só \n):
                    // sob raw mode o terminal não traduz \n em CR+LF, então sem o
                    // \r as linhas sairiam em escada.
                    print!("\r\x1b[K[{}] {}\r\n", u.timestamp, u.text);
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
            args.mic.clone(),
            args.system.clone(),
        )
        .await
    } else {
        crate::audio::recording_commands::start_recording_with_meeting_name(
            app.clone(),
            Some(effective_name.clone()),
            args.mic.clone(),
            args.system.clone(),
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

    // (7b) Banner de início. --mic/--system são os dispositivos realmente usados
    // (passados para a resolução acima); sem eles, vale a preferência ou o padrão do SO.
    let mic_label = args.mic.clone().unwrap_or_else(|| "default (OS)".to_string());
    let sys_label = args.system.clone().unwrap_or_else(|| "default (OS)".to_string());
    let lang_label = args
        .language
        .as_ref()
        .map(|l| format!("   ✓ Language: {}", l))
        .unwrap_or_default();
    // Aceleração compilada no binário (cfg de feature). Com o build CUDA e
    // use_gpu habilitado, o whisper sobe o modelo na GPU.
    let accel = if cfg!(feature = "cuda") {
        "CUDA"
    } else if cfg!(feature = "vulkan") {
        "Vulkan"
    } else if cfg!(target_os = "macos") {
        "Metal"
    } else {
        "CPU"
    };
    println!(
        "✓ Engine: {} [{}]   ✓ Mic: {}   ✓ System: {}{}",
        engine_label, accel, mic_label, sys_label, lang_label
    );
    println!(
        "✓ Meeting: \"{}\"  → {}",
        effective_name,
        folder_path.clone().unwrap_or_else(|| "(folder to be created)".into())
    );

    println!("Recording. Press 'p' to pause/resume, 'n' to add a note, Ctrl+C (or 'q') to stop and save.");

    // Details/observations buffer: seeded by --notes, appended live with the 'n' key,
    // and flushed into the meeting's AI summary context on stop (same field the app
    // shows on the meeting details page). `entering_note` freezes the live panel while
    // the user types a note so the prompt is not clobbered by the redraw.
    let notes = Arc::new(Mutex::new(args.notes.clone().unwrap_or_default()));
    let entering_note = Arc::new(AtomicBool::new(false));

    // Draw task do painel vivo: redesenha ~4x/s na própria linha, sempre (mesmo com
    // --quiet; quiet só esconde o texto dos transcripts). Toggla o ponto a cada ~1s e
    // rastreia silêncio (mic+sis abaixo de 0.01) para o alerta.
    // Flag de pausa compartilhada entre a task de teclado (quem alterna) e a draw
    // task (que congela o tempo enquanto pausado).
    let paused = Arc::new(AtomicBool::new(false));

    let draw_handle = {
        let panel = panel.clone();
        let record_only = args.record_only;
        let folder_for_bytes = folder_path.clone();
        let paused = paused.clone();
        let entering_note = entering_note.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let mut silent_accum_ms: u64 = 0;
            // Tempo total pausado (250ms por tick em pausa): subtraído do elapsed
            // para o cronômetro do painel congelar durante a pausa.
            let mut paused_accum_ms: u64 = 0;
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                interval.tick().await;
                // Enquanto o usuário digita uma nota ('n'), congela o painel para não
                // sobrescrever o prompt "note>" na mesma linha.
                if entering_note.load(Ordering::SeqCst) {
                    continue;
                }
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
                // Margem de 1 coluna: escrever na última célula faz wrap de borda em
                // alguns terminais — reservar evita reincidência do scroll infinito.
                let cols = terminal_size::terminal_size()
                    .map(|(terminal_size::Width(w), _)| (w as usize).saturating_sub(1));
                let is_paused = paused.load(Ordering::SeqCst);
                if is_paused {
                    paused_accum_ms = paused_accum_ms.saturating_add(250);
                }
                let line = {
                    let Ok(mut p) = panel.lock() else { continue };
                    p.paused = is_paused;
                    // Congela o cronômetro durante a pausa subtraindo o tempo pausado.
                    let elapsed = started
                        .elapsed()
                        .as_secs()
                        .saturating_sub(paused_accum_ms / 1000);
                    p.elapsed_secs = elapsed;
                    p.pulse_on = (elapsed % 2) == 0;
                    if record_only {
                        p.bytes_written = bytes;
                    }
                    // Silêncio: acumula 250ms por tick quando ambos rms < 0.01. Em
                    // pausa não há áudio por definição — zera para não disparar o
                    // alerta de "no audio".
                    if is_paused {
                        silent_accum_ms = 0;
                    } else if p.mic_rms < 0.01 && p.system_rms < 0.01 {
                        silent_accum_ms = silent_accum_ms.saturating_add(250);
                    } else {
                        silent_accum_ms = 0;
                    }
                    p.silent_secs = silent_accum_ms / 1000;
                    crate::cli::panel::render_fit(&p, cols)
                };
                print!("\r\x1b[K{}", line);
                let _ = std::io::stdout().flush();
            }
        })
    };

    // Pause/resume ao vivo: tecla 'p' alterna; Ctrl+C ou 'q' encerra. Lemos o teclado
    // char-a-char (raw mode), igual ao botão de pausa da GUI. Sem TTY (pipe/redirect),
    // `RawModeGuard::enable` devolve None e caímos no fluxo clássico (Ctrl+C/--duration).
    match RawModeGuard::enable() {
        Some(raw_guard) => {
            // Task de teclado (blocking) → loop async via canal. O poll com timeout
            // permite encerrar a task assim que `stop_keys` for setado.
            let (key_tx, mut key_rx) = tokio::sync::mpsc::unbounded_channel::<KeyCmd>();
            let stop_keys = Arc::new(AtomicBool::new(false));
            let key_handle = {
                let stop_keys = stop_keys.clone();
                let notes_kb = notes.clone();
                let entering_note_kb = entering_note.clone();
                tokio::task::spawn_blocking(move || {
                    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
                    loop {
                        if stop_keys.load(Ordering::SeqCst) {
                            break;
                        }
                        match event::poll(std::time::Duration::from_millis(200)) {
                            Ok(true) => {
                                if let Ok(Event::Key(k)) = event::read() {
                                    // Windows emite Press E Release; só agimos no Press.
                                    if k.kind != KeyEventKind::Press {
                                        continue;
                                    }
                                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                                    match k.code {
                                        KeyCode::Char('c') if ctrl => {
                                            let _ = key_tx.send(KeyCmd::Stop);
                                            break;
                                        }
                                        KeyCode::Char('p') | KeyCode::Char('P') if !ctrl => {
                                            let _ = key_tx.send(KeyCmd::TogglePause);
                                        }
                                        KeyCode::Char('q') | KeyCode::Char('Q') if !ctrl => {
                                            let _ = key_tx.send(KeyCmd::Stop);
                                            break;
                                        }
                                        KeyCode::Char('n') | KeyCode::Char('N') if !ctrl => {
                                            // Captura uma linha de nota inline (raw mode não
                                            // ecoa; ecoamos char a char). Congela o painel via
                                            // `entering_note` enquanto o usuário digita.
                                            entering_note_kb.store(true, Ordering::SeqCst);
                                            print!("\r\x1b[Knote> ");
                                            let _ = std::io::stdout().flush();
                                            let mut buf = String::new();
                                            let mut abort_stop = false;
                                            loop {
                                                // Aborta a nota se um stop foi pedido enquanto
                                                // digita (ex.: --duration expira), senão o
                                                // `key_handle.await` do stop travaria aqui.
                                                if stop_keys.load(Ordering::SeqCst) {
                                                    buf.clear();
                                                    break;
                                                }
                                                match event::poll(
                                                    std::time::Duration::from_millis(200),
                                                ) {
                                                    Ok(true) => {}
                                                    Ok(false) => continue,
                                                    Err(_) => break,
                                                }
                                                match event::read() {
                                                    Ok(Event::Key(ke)) => {
                                                        if ke.kind != KeyEventKind::Press {
                                                            continue;
                                                        }
                                                        let ictrl = ke
                                                            .modifiers
                                                            .contains(KeyModifiers::CONTROL);
                                                        match ke.code {
                                                            // Enter: confirma a nota.
                                                            KeyCode::Enter => break,
                                                            // Esc: cancela a nota.
                                                            KeyCode::Esc => {
                                                                buf.clear();
                                                                break;
                                                            }
                                                            // Ctrl+C durante a nota: encerra a gravação.
                                                            KeyCode::Char('c') if ictrl => {
                                                                abort_stop = true;
                                                                break;
                                                            }
                                                            KeyCode::Backspace => {
                                                                if buf.pop().is_some() {
                                                                    print!("\u{8} \u{8}");
                                                                    let _ = std::io::stdout().flush();
                                                                }
                                                            }
                                                            KeyCode::Char(c) if !ictrl => {
                                                                buf.push(c);
                                                                print!("{}", c);
                                                                let _ = std::io::stdout().flush();
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                    Ok(_) => {}
                                                    Err(_) => break,
                                                }
                                            }
                                            let trimmed = buf.trim();
                                            if !trimmed.is_empty() {
                                                if let Ok(mut n) = notes_kb.lock() {
                                                    if !n.is_empty() {
                                                        n.push('\n');
                                                    }
                                                    n.push_str(trimmed);
                                                }
                                                print!("\r\x1b[Knote saved\r\n");
                                            } else {
                                                print!("\r\x1b[K");
                                            }
                                            let _ = std::io::stdout().flush();
                                            entering_note_kb.store(false, Ordering::SeqCst);
                                            if abort_stop {
                                                let _ = key_tx.send(KeyCmd::Stop);
                                                break;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Ok(false) => {}
                            Err(_) => break,
                        }
                    }
                })
            };

            // Espera: --duration OU um comando da task de teclado.
            let sleep = async {
                match args.duration {
                    Some(secs) => {
                        tokio::time::sleep(std::time::Duration::from_secs(secs)).await
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    cmd = key_rx.recv() => match cmd {
                        None | Some(KeyCmd::Stop) => break,
                        Some(KeyCmd::TogglePause) => {
                            let want_paused = !paused.load(Ordering::SeqCst);
                            let res = if want_paused {
                                crate::audio::recording_commands::pause_recording(app.clone()).await
                            } else {
                                crate::audio::recording_commands::resume_recording(app.clone()).await
                            };
                            match res {
                                Ok(()) => paused.store(want_paused, Ordering::SeqCst),
                                // Não derruba a gravação; só avisa (raro: corrida de estado).
                                Err(e) => {
                                    print!("\r\x1b[K⚠ pause/resume failed: {}\r\n", e);
                                    let _ = std::io::stdout().flush();
                                }
                            }
                        }
                    },
                }
            }

            // Encerra a task de teclado e RESTAURA o terminal ANTES dos prints de
            // shutdown (que usam \n e precisam do terminal em modo cozido de volta).
            stop_keys.store(true, Ordering::SeqCst);
            let _ = key_handle.await;
            drop(raw_guard);
        }
        None => {
            // Sem TTY: comportamento clássico, sem pause.
            wait_for_stop(args.duration).await;
        }
    }

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

    // Flush details/observations (--notes seed + live 'n' notes) into the meeting's
    // AI summary context — the same field editable in the app. Done before --summarize
    // so the notes feed the summary. A failure here is a warning only (the meeting is
    // already persisted).
    let notes_text = notes
        .lock()
        .map(|n| n.trim().to_string())
        .unwrap_or_default();
    if !notes_text.is_empty() && meeting_id != "?" {
        match crate::summary::context::commands::api_save_summary_context(
            app.state::<AppState>(),
            meeting_id.to_string(),
            notes_text,
        )
        .await
        {
            Ok(()) => println!("✓ Notes saved to the meeting context."),
            Err(e) => eprintln!("Warning: could not save notes: {}", e),
        }
    }

    let folder_display = folder_path
        .clone()
        .unwrap_or_else(|| "(no folder)".to_string());
    if args.record_only && args.retranscribe_model.is_none() {
        println!(
            "✓ Recorded (no AI). meeting_id={} folder={}  Re-transcribe in the app whenever you want.",
            meeting_id, folder_display
        );
    } else {
        println!("✓ Saved. meeting_id={} folder={}", meeting_id, folder_display);
    }

    // record --retranscribe-model: re-runs the transcription from the saved audio with a
    // better model before the summary. The id used here comes from THIS run, so several
    // CLI processes may overlap without stepping on each other, unlike `summarize --last`,
    // which resolves to whichever meeting finished most recently.
    //
    // `start_retranscription` replaces the meeting's transcripts (DELETE + INSERT in one
    // transaction), so the summary below reads only the new text, and the explicit model
    // wins over the app config (`get_or_init_whisper` honours the requested model).
    if let Some(retranscribe_model) = args.retranscribe_model.clone() {
        if meeting_id == "?" {
            return Err(
                "--retranscribe-model needs the meeting id, which the database did not return."
                    .to_string(),
            );
        }
        let folder = folder_path.clone().ok_or_else(|| {
            "--retranscribe-model needs the meeting folder, which was not created.".to_string()
        })?;

        retranscribe_meeting(
            app,
            meeting_id,
            folder,
            retranscribe_model,
            // Engine: explicit flag, else the recording engine, else whisper.
            args.retranscribe_engine
                .clone()
                .or_else(|| args.engine.clone()),
            args.retranscribe_language
                .clone()
                .or_else(|| args.language.clone()),
        )
        .await?;
    }

    // record --summarize: chains the summary of the meeting just recorded. validate()
    // guarantees there is a transcript to work with: either the live one, or the one
    // --retranscribe-model produced above (the only way --record-only gets here).
    // A failure here is a warning only: the recording is already persisted, so the
    // command still exits successfully.
    if args.summarize {
        match summarize_existing_meeting(app, meeting_id, args.template.clone()).await {
            Ok(()) => println!("✓ Summary saved. meeting_id={}", meeting_id),
            Err(e) => eprintln!(
                "Warning: --summarize failed (the recording was saved): {}",
                e
            ),
        }
    }

    Ok(())
}

/// Lista as reuniões (id, data, título), mais recentes primeiro, para descobrir
/// os ids usados em `summarize --meeting <id>`.
pub async fn run_list_meetings(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::database::repositories::meeting::MeetingsRepository;

    let pool = app.state::<AppState>().db_manager.pool().clone();
    let meetings = MeetingsRepository::get_meetings(&pool)
        .await
        .map_err(|e| format!("Failed to list meetings: {}", e))?;

    if meetings.is_empty() {
        println!("(none)");
        return Ok(());
    }
    for m in meetings {
        println!(
            "{}  {}  {}",
            m.id,
            m.created_at.0.format("%Y-%m-%d %H:%M"),
            m.title
        );
    }
    Ok(())
}

/// Lista os templates de sumário disponíveis (built-in + custom): id, nome e
/// descrição, para uso em `summarize --template <id>`.
pub async fn run_list_templates() -> Result<(), String> {
    let templates = crate::summary::templates::list_templates();
    if templates.is_empty() {
        println!("(none)");
        return Ok(());
    }
    for (id, name, description) in templates {
        println!("- {}  —  {}: {}", id, name, description);
    }
    Ok(())
}

/// Núcleo da geração de sumário pela CLI: espelha `api_process_transcript`, mas
/// AGUARDA o processamento (em vez de `spawn`). Mostra um spinner com tempo
/// decorrido enquanto o LLM trabalha e, ao final, lê o status persistido para
/// confirmar (`completed`) ou propagar o erro do banco.
pub async fn summarize_meeting(
    app: &tauri::AppHandle,
    meeting_id: &str,
    text: String,
    provider: String,
    model: String,
    template_id: String,
) -> Result<(), String> {
    use crate::database::repositories::summary::SummaryProcessesRepository;
    use crate::database::repositories::transcript_chunk::TranscriptChunksRepository;
    use crate::summary::service::SummaryService;

    let pool = app.state::<AppState>().db_manager.pool().clone();

    // (1) cria/zera o processo e (2) grava os chunks — igual a api_process_transcript.
    SummaryProcessesRepository::create_or_reset_process(&pool, meeting_id)
        .await
        .map_err(|e| format!("Failed to initialize the summary process: {}", e))?;

    TranscriptChunksRepository::save_transcript_data(
        &pool, meeting_id, &text, &provider, &model, 40000, 1000,
    )
    .await
    .map_err(|e| format!("Failed to save transcript data: {}", e))?;

    // Spinner com tempo decorrido (mesmo padrão da draw task do run_record).
    let draw_handle = tokio::spawn(async move {
        let started = std::time::Instant::now();
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let mut i = 0usize;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            interval.tick().await;
            let f = frames[i % frames.len()];
            i += 1;
            print!(
                "\r\x1b[K{} Generating summary… {}s",
                f,
                started.elapsed().as_secs()
            );
            let _ = std::io::stdout().flush();
        }
    });

    // Aguarda o processamento (persiste status/result no banco internamente).
    SummaryService::process_transcript_background(
        app.clone(),
        pool.clone(),
        meeting_id.to_string(),
        text,
        provider,
        model,
        template_id,
    )
    .await;

    // Para o spinner e limpa a linha.
    draw_handle.abort();
    print!("\r\x1b[K");
    let _ = std::io::stdout().flush();

    // Confirma pelo status persistido.
    match SummaryProcessesRepository::get_summary_data(&pool, meeting_id).await {
        Ok(Some(p)) if p.status.to_lowercase() == "completed" => Ok(()),
        Ok(Some(p)) => Err(p
            .error
            .unwrap_or_else(|| format!("summary did not complete (status: {})", p.status))),
        Ok(None) => Err("summary process not found after generation.".to_string()),
        Err(e) => Err(format!("failed to read the summary status: {}", e)),
    }
}

/// Re-transcribes a meeting's already-saved audio with another model, REPLACING its
/// transcripts (`start_retranscription` does DELETE + INSERT in one transaction).
/// Shared by `record --retranscribe-model` and by the `retranscribe` subcommand.
///
/// `engine` takes whisper|localWhisper|parakeet; anything else falls back to whisper
/// with a warning. `language` as `None` lets the engine use the app preference.
///
/// Ctrl+C cancels and exits with 130 on purpose: whatever is chained after this
/// (the summary) must not run on top of the old transcript.
async fn retranscribe_meeting(
    app: &tauri::AppHandle,
    meeting_id: &str,
    folder: String,
    model: String,
    engine: Option<String>,
    language: Option<String>,
) -> Result<(), String> {
    let provider = match engine.as_deref() {
        Some("parakeet") => "parakeet",
        Some("whisper") | Some("localWhisper") | None => "whisper",
        Some(other) => {
            eprintln!("Warning: unknown engine '{}'; using whisper.", other);
            "whisper"
        }
    };

    // The engine global must exist: `get_or_init_whisper` LOADS a model but does not
    // CREATE the engine, it errors with "not initialized" instead. In the app, setup()
    // does this; in the CLI it only happens lazily during live transcription, so it is
    // still None under --record-only, and it is also None for the engine that did not
    // record (e.g. recorded with parakeet, re-transcribing with whisper). Both inits are
    // idempotent no-ops when the engine already exists.
    let init = if provider == "parakeet" {
        crate::parakeet_engine::commands::parakeet_init().await
    } else {
        crate::whisper_engine::commands::whisper_init().await
    };
    init.map_err(|e| format!("Failed to initialize the {} engine: {}", provider, e))?;

    println!(
        "⟳ Re-transcribing with {}/{} (press Ctrl+C to cancel; anything chained after it is skipped).",
        provider, model
    );

    // Progress feed: the job emits "retranscription-progress" while decoding and
    // transcribing. Without it a large model over a long meeting looks frozen for
    // minutes, and the user cannot tell whether their Ctrl+C registered.
    let progress_listener = {
        use tauri::Listener;
        app.listen("retranscription-progress", move |e: tauri::Event| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
                let msg = v["message"].as_str().unwrap_or_default();
                let pct = v["progress_percentage"].as_u64().unwrap_or(0);
                print!("\r\x1b[K⏳ {:>3}% {}", pct, msg);
                let _ = std::io::stdout().flush();
            }
        })
    };

    // Ctrl+C here only RAISES the cancellation flag: the job checks it between chunks and
    // unwinds on its own, unloading the engine. Racing it with `tokio::select!` would drop
    // the future instead, leaving the blocking Whisper task and the loaded model behind.
    // A second Ctrl+C gives up on waiting for that unwind.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_watch = {
        let cancelled = cancelled.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancelled.store(true, Ordering::SeqCst);
                println!();
                println!("⚠ Cancelling the re-transcription (press Ctrl+C again to abandon)...");
                crate::audio::retranscription::cancel_retranscription();
            }
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!();
                eprintln!(
                    "⚠ Abandoned during the re-transcription; the meeting keeps the transcript \
                     it already had."
                );
                std::process::exit(130);
            }
        })
    };

    let result = crate::audio::retranscription::start_retranscription(
        app.clone(),
        meeting_id.to_string(),
        folder,
        language,
        Some(model.clone()),
        Some(provider.to_string()),
    )
    .await;
    cancel_watch.abort();
    {
        use tauri::Listener;
        app.unlisten(progress_listener);
    }
    print!("\r\x1b[K");
    let _ = std::io::stdout().flush();

    match result {
        Ok(res) => {
            println!(
                "✓ Re-transcribed with {}/{}: {} segments over {:.1}s. meeting_id={}",
                provider, model, res.segments_count, res.duration_seconds, meeting_id
            );
            Ok(())
        }
        // Cancelled: the meeting and its previous transcript are intact. Exiting here is
        // what keeps a chained --summarize from running: summarizing the old text after
        // the user asked for a better one would silently hand back the worse result.
        Err(_) if cancelled.load(Ordering::SeqCst) => {
            eprintln!(
                "⚠ Re-transcription cancelled; nothing chained after it ran. meeting_id={}",
                meeting_id
            );
            std::process::exit(130);
        }
        // Failed: same reasoning, stop instead of falling through to the old transcript.
        Err(e) => Err(format!(
            "Re-transcription failed (the meeting is intact; retry with \
             `retranscribe --meeting {}` or in the app): {}",
            meeting_id, e
        )),
    }
}

/// Re-transcribes an EXISTING meeting (`--meeting <id>` or `--last`) and, with
/// `--summarize`, chains the summary right after it.
pub async fn run_retranscribe(
    app: &tauri::AppHandle,
    args: RetranscribeArgs,
) -> Result<(), String> {
    use crate::database::repositories::meeting::{MeetingsRepository, STATUS_COMPLETED};

    args.validate()?;

    let pool = app.state::<AppState>().db_manager.pool().clone();

    let meeting_id = if let Some(id) = args.meeting.clone() {
        id
    } else {
        // --last: the most recent one (get_meetings already orders by created_at DESC).
        // Racy when CLI runs overlap, hence the hint in the help to pass --meeting <id>.
        let meetings = MeetingsRepository::get_meetings(&pool)
            .await
            .map_err(|e| format!("Failed to list meetings: {}", e))?;
        meetings
            .first()
            .map(|m| m.id.clone())
            .ok_or_else(|| "No meetings found.".to_string())?
    };

    // The meeting folder holds the audio; without it there is nothing to re-transcribe.
    let folder = match MeetingsRepository::get_meeting_metadata(&pool, &meeting_id).await {
        Ok(Some(m)) => {
            // Unlike get_meetings (which filters on STATUS_COMPLETED), get_meeting_metadata
            // returns rows that are still recording. Re-transcribing one would DELETE the
            // transcripts of a live session and read a half-written audio file. Very much
            // reachable when a second CLI is handed the id of a recording still in flight.
            if m.status != STATUS_COMPLETED {
                return Err(format!(
                    "Meeting {} is still being recorded (status: {}); wait for it to finish.",
                    meeting_id, m.status
                ));
            }
            m.folder_path.ok_or_else(|| {
                format!(
                    "Meeting {} has no folder on disk; there is no audio to re-transcribe.",
                    meeting_id
                )
            })?
        }
        Ok(None) | Err(sqlx::Error::RowNotFound) => {
            return Err(format!("Meeting {} not found.", meeting_id))
        }
        Err(e) => return Err(format!("Failed to load the meeting: {}", e)),
    };

    retranscribe_meeting(
        app,
        &meeting_id,
        folder,
        args.model.clone(),
        args.engine.clone(),
        args.language.clone(),
    )
    .await?;

    if args.summarize {
        summarize_existing_meeting(app, &meeting_id, args.template.clone()).await?;
        println!("✓ Summary saved. meeting_id={}", meeting_id);
    }

    Ok(())
}

/// Resolve transcript + template + provider/model de uma reunião EXISTENTE e
/// gera o sumário. Compartilhado por `run_summarize` e por `record --summarize`.
async fn summarize_existing_meeting(
    app: &tauri::AppHandle,
    meeting_id: &str,
    template: Option<String>,
) -> Result<(), String> {
    use crate::database::repositories::{
        meeting::MeetingsRepository, setting::SettingsRepository,
    };

    let pool = app.state::<AppState>().db_manager.pool().clone();

    // Transcript concatenado. get_meeting devolve Err(RowNotFound) (não Ok(None))
    // quando o id não existe — tratamos esse caso como "not found" amigável.
    let details = match MeetingsRepository::get_meeting(&pool, meeting_id).await {
        Ok(Some(d)) => d,
        Ok(None) | Err(sqlx::Error::RowNotFound) => {
            return Err(format!("Meeting {} not found.", meeting_id))
        }
        Err(e) => return Err(format!("Failed to load the meeting: {}", e)),
    };
    let text = details
        .transcripts
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return Err(
            "This meeting has no transcript to summarize; re-transcribe it in the app."
                .to_string(),
        );
    }

    // Template (default = daily_standup); valida e, se inválido, lista os ids.
    let template_id = template.unwrap_or_else(|| "daily_standup".to_string());
    if crate::summary::templates::get_template(&template_id).is_err() {
        let ids: Vec<String> = crate::summary::templates::list_templates()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        return Err(format!(
            "Unknown template '{}'. Available: {}",
            template_id,
            ids.join(", ")
        ));
    }

    // Provider/model SEMPRE da config do app (settings).
    let (provider, model) = match SettingsRepository::get_model_config(&pool).await {
        Ok(Some(s)) if !s.provider.is_empty() && !s.model.is_empty() => (s.provider, s.model),
        _ => {
            return Err(
                "Summary model not configured. Configure it in the app first.".to_string(),
            )
        }
    };

    summarize_meeting(app, meeting_id, text, provider, model, template_id).await
}

/// Gera o sumário de uma reunião existente, escolhida por `--meeting <id>` ou
/// `--last`. Só salva (banco + summary.md); não imprime o markdown.
pub async fn run_summarize(app: &tauri::AppHandle, args: SummarizeArgs) -> Result<(), String> {
    use crate::database::repositories::meeting::MeetingsRepository;

    args.validate()?;

    let meeting_id = if let Some(id) = args.meeting.clone() {
        id
    } else {
        // --last: mais recente (get_meetings já ordena por created_at DESC).
        let pool = app.state::<AppState>().db_manager.pool().clone();
        let meetings = MeetingsRepository::get_meetings(&pool)
            .await
            .map_err(|e| format!("Failed to list meetings: {}", e))?;
        meetings
            .first()
            .map(|m| m.id.clone())
            .ok_or_else(|| "No meetings found.".to_string())?
    };

    summarize_existing_meeting(app, &meeting_id, args.template.clone()).await?;

    println!("✓ Summary saved. meeting_id={}", meeting_id);
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
