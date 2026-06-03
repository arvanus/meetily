use app_lib::cli::args::{Cli, Command};
use app_lib::cli::runner;
use clap::Parser;

fn main() {
    std::env::set_var("RUST_LOG", "info");
    let _ = env_logger::try_init();

    let cli = Cli::parse();
    let app = runner::build_headless_app().expect("falha ao construir o app headless");
    let handle = app.handle().clone();

    tauri::async_runtime::block_on(async {
        runner::init_database(&handle).await.expect("falha ao iniciar o banco");
    });

    // Resolve os diretórios de modelos a partir do AppHandle antes de usar os comandos.
    app_lib::whisper_engine::commands::set_models_directory(&handle);
    app_lib::parakeet_engine::commands::set_models_directory(&handle);

    match cli.command.unwrap_or(Command::ListDevices) {
        Command::ListDevices => {
            tauri::async_runtime::block_on(runner::run_list_devices())
                .unwrap_or_else(|e| eprintln!("erro: {e}"));
        }
        Command::ListModels => {
            tauri::async_runtime::block_on(runner::run_list_models(&handle))
                .unwrap_or_else(|e| eprintln!("erro: {e}"));
        }
        Command::Record(args) => {
            if let Err(e) = tauri::async_runtime::block_on(runner::run_record(&handle, args)) {
                eprintln!("erro: {e}");
                std::process::exit(1);
            }
        }
    }
}
