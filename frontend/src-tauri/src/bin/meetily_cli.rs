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

    match cli.command.unwrap_or(Command::ListDevices) {
        Command::ListDevices => println!("(list-devices: implementado na Task 2)"),
        Command::ListModels => println!("(list-models: implementado na Task 2)"),
        Command::Record(_args) => println!("(record: implementado na Task 3)"),
    }
}
