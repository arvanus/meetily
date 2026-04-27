#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use log;
use env_logger;

const APP_IDENTIFIER: &str = "com.meetily.ai";

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");
        let backtrace = std::backtrace::Backtrace::force_capture();

        let entry = format!(
            "===== PANIC at {ts} =====\n\
             thread: {thread}\n\
             {info}\n\n\
             backtrace:\n{bt}\n\n",
            ts = timestamp,
            thread = thread_name,
            info = info,
            bt = backtrace,
        );

        if let Some(base) = dirs::data_dir() {
            let dir = base.join(APP_IDENTIFIER);
            if std::fs::create_dir_all(&dir).is_ok() {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join("panic.log"))
                {
                    use std::io::Write;
                    let _ = f.write_all(entry.as_bytes());
                    let _ = f.flush();
                }
            }
        }

        log::error!("{}", entry);
        eprintln!("{}", entry);

        default_hook(info);
    }));
}

fn main() {
    std::env::set_var("RUST_BACKTRACE", "full");
    install_panic_hook();

    std::env::set_var("RUST_LOG", "info");
    env_logger::init();

    // Async logger will be initialized lazily when first needed (after Tauri runtime starts)
    log::info!("Starting application...");
    app_lib::run();
}
