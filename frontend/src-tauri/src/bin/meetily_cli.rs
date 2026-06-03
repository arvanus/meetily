use app_lib::cli::args::{Cli, Command};
use app_lib::cli::runner;
use clap::Parser;

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

    let cli = Cli::parse();
    let app = runner::build_headless_app().expect("failed to build the headless app");
    let handle = app.handle().clone();

    tauri::async_runtime::block_on(async {
        runner::init_database(&handle).await.expect("failed to initialize the database");
    });

    // Resolve os diretórios de modelos a partir do AppHandle antes de usar os comandos.
    app_lib::whisper_engine::commands::set_models_directory(&handle);
    app_lib::parakeet_engine::commands::set_models_directory(&handle);

    match cli.command.unwrap_or(Command::ListDevices) {
        Command::ListDevices => {
            tauri::async_runtime::block_on(runner::run_list_devices())
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::ListModels => {
            tauri::async_runtime::block_on(runner::run_list_models(&handle))
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::Record(args) => {
            if let Err(e) = tauri::async_runtime::block_on(runner::run_record(&handle, args)) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}
