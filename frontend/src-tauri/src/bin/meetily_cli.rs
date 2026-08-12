use app_lib::cli::args::{Cli, Command, RecordArgs};
use app_lib::cli::runner;
use clap::Parser;
use tauri::Manager;

fn main() {
    // Logs do framework são ruído no CLI: a transcrição ao vivo e o painel usam stdout
    // direto (println!), então deixamos o log padrão enxuto (só warn/erro) e silenciamos
    // o erro inofensivo de bandeja (`app_lib::tray`), que não existe no modo headless.
    // O usuário pode sobrescrever exportando RUST_LOG (ex.: RUST_LOG=info para depurar).
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "warn,app_lib::tray=off");
    }
    let _ = env_logger::try_init();
    // Redireciona os logs em C do whisper.cpp/ggml (whisper_init_state, ggml_*,
    // register_backend, ...) para o `log` do Rust — com o default acima (warn),
    // esse ruído some e deixa de quebrar a linha de status do painel.
    whisper_rs::install_whisper_log_trampoline();
    // Same idea for ALSA: enumerating devices makes libasound write its plugin probes to
    // stderr, and the device monitor enumerates on a loop while recording, which shreds
    // the status panel. Nothing is lost - the messages report plugins ALSA rejects itself.
    #[cfg(target_os = "linux")]
    app_lib::audio::devices::platform::silence_alsa_logging();

    let cli = Cli::parse();
    let app = runner::build_headless_app().expect("failed to build the headless app");
    let handle = app.handle().clone();

    tauri::async_runtime::block_on(async {
        runner::init_database(&handle).await.expect("failed to initialize the database");
    });

    // Resolve os diretórios de modelos a partir do AppHandle antes de usar os comandos.
    app_lib::whisper_engine::commands::set_models_directory(&handle);
    app_lib::parakeet_engine::commands::set_models_directory(&handle);

    // build_headless_app() only calls .build(), not .run() — lib.rs's setup() (which
    // registers the templates dir via resource_dir) never fires. Without this,
    // list-templates only sees the 2 templates hardcoded in defaults.rs.
    if let Ok(resource_path) = handle.path().resource_dir() {
        let templates_dir = resource_path.join("templates");
        app_lib::summary::templates::set_bundled_templates_dir(templates_dir);
    } else {
        log::warn!("Failed to resolve resource directory for templates");
    }

    // Custom templates live under app_data_dir, not in the bundle, so that a user's edits
    // survive reinstalling the app. The CLI has to resolve them for the same reason it has
    // to resolve the bundled ones above.
    match handle.path().app_data_dir() {
        Ok(app_data_dir) => {
            app_lib::summary::templates::set_custom_templates_dir(app_data_dir.join("templates"))
        }
        Err(e) => log::warn!(
            "Failed to resolve app data directory for custom templates: {}",
            e
        ),
    }

    // Sem subcomando = gravar com a config padrão do app (Record é o default).
    match cli.command.unwrap_or(Command::Record(RecordArgs::default())) {
        Command::ListDevices => {
            tauri::async_runtime::block_on(runner::run_list_devices())
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::ListModels => {
            tauri::async_runtime::block_on(runner::run_list_models(&handle))
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::ListMeetings => {
            tauri::async_runtime::block_on(runner::run_list_meetings(&handle))
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::ListTemplates => {
            tauri::async_runtime::block_on(runner::run_list_templates())
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::Retranscribe(args) => {
            if let Err(e) = tauri::async_runtime::block_on(runner::run_retranscribe(&handle, args))
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Summarize(args) => {
            if let Err(e) = tauri::async_runtime::block_on(runner::run_summarize(&handle, args)) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Command::Record(args) => {
            if let Err(e) = tauri::async_runtime::block_on(runner::run_record(&handle, args)) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}
