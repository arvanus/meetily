# meetily-cli (gravação headless) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Um binário `meetily-cli` que grava e transcreve uma reunião sem abrir a UI, mostra feedback ao vivo no terminal, e persiste exatamente como o app (pasta + SQLite), incluindo um modo `--record-only` sem IA.

**Architecture:** Novo `[[bin]]` no crate `app_lib` que sobe um `tauri::Builder` **sem janela**, registra os mesmos `State` do app e roda no `setup()`. Reusa `start_recording_with_meeting_name`/`stop_recording` e os eventos `transcript-update`/`audio-levels`, consumindo-os com listeners Rust que imprimem no terminal. O DSP de FFT roda no Rust (onde as amostras vivem) e é incluído no payload de `audio-levels`; o CLI só renderiza.

**Tech Stack:** Rust, Tauri 2.x, `clap 4.3` (já dep), `realfft 3.4.0` (já dep), `tokio` (já dep), `sqlx`/sqlite (já dep). Identifier compartilhado: `com.meetily.ai`.

**Branch:** `test/all-features` (decisão do usuário — herda a config de build Windows/CUDA).

**Build/run do binário (referência):**
```
# compilar só o CLI (sem bundle), na pasta frontend/src-tauri
cargo build --bin meetily-cli                 # CPU
cargo build --bin meetily-cli --features cuda # NVIDIA
# rodar
cargo run --bin meetily-cli -- record --name "Teste"
cargo run --bin meetily-cli -- list-devices
```

---

## File Structure

- **Create** `frontend/src-tauri/src/bin/meetily_cli.rs` — entrypoint do CLI: parse de args (clap), build do app Tauri headless, dispatch de subcomandos. Responsabilidade única: orquestração/CLI.
- **Create** `frontend/src-tauri/src/cli/mod.rs` — módulo `cli` exportado pela lib (`app_lib::cli`), reutilizável e testável.
- **Create** `frontend/src-tauri/src/cli/args.rs` — definição clap (`Cli`, `Command`, `RecordArgs`). Puro, testável.
- **Create** `frontend/src-tauri/src/cli/panel.rs` — render do painel vivo (string a partir de um struct de estado). Puro, testável.
- **Create** `frontend/src-tauri/src/cli/runner.rs` — lógica headless: build do app, init de DB/modelos, start/stop, loop de listeners, persistência no DB.
- **Modify** `frontend/src-tauri/Cargo.toml` — adicionar `[[bin]]`; possivelmente `crossterm` (decisão na Task 4).
- **Modify** `frontend/src-tauri/src/lib.rs` — `pub mod cli;` para expor o módulo à lib e ao bin.
- **Modify** `frontend/src-tauri/src/audio/recording_commands.rs` — incluir `bands` no payload `audio-levels` (Task 5); novo caminho `start_recording_only` (Task 6).
- **Modify** `frontend/src-tauri/src/audio/recording_state.rs` (ou onde vive `RecordingState`) — expor janela recente de amostras p/ FFT, se ainda não existir (Task 5, após investigação).

> **Por que `cli/` na lib e não tudo no bin:** o bin (`src/bin/*.rs`) não é testável por `cargo test` com a mesma ergonomia; manter a lógica em `app_lib::cli` permite testes unitários (`args`, `panel`, mapeamento de bandas) e mantém o bin como casca fina.

---

## Task 1: Esqueleto do binário + headless Tauri que sobe e encerra

**Files:**
- Create: `frontend/src-tauri/src/cli/mod.rs`
- Create: `frontend/src-tauri/src/cli/args.rs`
- Create: `frontend/src-tauri/src/cli/runner.rs`
- Create: `frontend/src-tauri/src/bin/meetily_cli.rs`
- Modify: `frontend/src-tauri/Cargo.toml`
- Modify: `frontend/src-tauri/src/lib.rs`

- [ ] **Step 1: Adicionar o bin e expor o módulo cli**

Em `frontend/src-tauri/Cargo.toml`, logo após o bloco `[lib]` (linha ~16):

```toml
[[bin]]
name = "meetily-cli"
path = "src/bin/meetily_cli.rs"
```

Em `frontend/src-tauri/src/lib.rs`, junto aos outros `pub mod` no topo do arquivo:

```rust
pub mod cli;
```

- [ ] **Step 2: Definir os args (clap) com teste**

Create `frontend/src-tauri/src/cli/args.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "meetily-cli", about = "Grava e transcreve reuniões sem a UI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Grava uma reunião (subcomando padrão)
    Record(RecordArgs),
    /// Lista os modelos de transcrição disponíveis e o padrão
    ListModels,
    /// Lista os dispositivos de áudio
    ListDevices,
}

#[derive(clap::Args, Debug, Default, Clone)]
pub struct RecordArgs {
    /// Nome da reunião (default: "CLI Meeting <data/hora>")
    #[arg(long)]
    pub name: Option<String>,
    /// Grava SEM carregar IA; re-transcreva depois pelo app
    #[arg(long)]
    pub record_only: bool,
    /// Motor de transcrição (default: o configurado no app)
    #[arg(long)]
    pub engine: Option<String>,
    /// Modelo (default: o configurado no app)
    #[arg(long)]
    pub model: Option<String>,
    /// Microfone (default: padrão do SO)
    #[arg(long)]
    pub mic: Option<String>,
    /// Áudio do sistema (default: loopback padrão)
    #[arg(long)]
    pub system: Option<String>,
    /// Mostra transcrições parciais ao vivo
    #[arg(long)]
    pub partial: bool,
    /// Não imprime transcrição (o painel vivo continua)
    #[arg(long)]
    pub quiet: bool,
    /// Para automaticamente após N segundos
    #[arg(long)]
    pub duration: Option<u64>,
    /// Só transcreve, não grava o .mp4 (incompatível com --record-only)
    #[arg(long)]
    pub no_audio_save: bool,
}

impl RecordArgs {
    /// Valida combinações inválidas de flags.
    pub fn validate(&self) -> Result<(), String> {
        if self.record_only && self.no_audio_save {
            return Err(
                "--record-only requer salvar o áudio (é a fonte da re-transcrição); \
                 remova --no-audio-save."
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn record_only_with_no_audio_save_is_rejected() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only", "--no-audio-save"]);
        let Some(Command::Record(args)) = cli.command else { panic!("esperava Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn plain_record_only_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--record-only"]);
        let Some(Command::Record(args)) = cli.command else { panic!("esperava Record") };
        assert!(args.validate().is_ok());
        assert!(args.record_only);
    }
}
```

Create `frontend/src-tauri/src/cli/mod.rs`:

```rust
pub mod args;
pub mod panel;
pub mod runner;
```

(Crie `panel.rs` e `runner.rs` como stubs vazios agora para compilar; serão preenchidos nas tasks seguintes.)

`frontend/src-tauri/src/cli/panel.rs`:
```rust
// Preenchido na Task 4.
```

`frontend/src-tauri/src/cli/runner.rs`:
```rust
// Preenchido na Task 1, Step 4.
```

- [ ] **Step 3: Rodar o teste de args (deve falhar a compilar, depois passar)**

Run: `cargo test --lib cli::args`
Expected: primeiro FAIL/compile-error enquanto `panel.rs`/`runner.rs` não existem; após criar os stubs e o código, PASS nos dois testes.

- [ ] **Step 4: Runner mínimo — sobe app headless, inicializa DB, encerra**

Preencher `frontend/src-tauri/src/cli/runner.rs`:

```rust
use crate::state::AppState;
use crate::database::manager::DatabaseManager;
use tauri::Manager;

/// Constrói um app Tauri SEM janela. Reusa os mesmos State do app (lib.rs:441-446)
/// para que os comandos de gravação não entrem em pânico ao buscar `tauri::State`.
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
```

> **Nota de integração:** se `generate_context!()` exigir uma janela em `tauri.conf.json`,
> NÃO criar janela aqui — apenas não chamar `.run()`/não abrir webview. Validar no Step 6.
> Se o build reclamar de window obrigatória, a saída é usar `app.run(|_,_| {})` e nunca
> instanciar `WebviewWindowBuilder` (o app não abre janela porque não a criamos no setup).

- [ ] **Step 5: Bin entrypoint mínimo**

Create `frontend/src-tauri/src/bin/meetily_cli.rs`:

```rust
use app_lib::cli::args::{Cli, Command};
use app_lib::cli::runner;
use clap::Parser;
use tauri::Manager;

fn main() {
    std::env::set_var("RUST_LOG", "info");
    let _ = env_logger::try_init();

    let cli = Cli::parse();
    let app = runner::build_headless_app().expect("falha ao construir o app headless");
    let handle = app.handle().clone();

    // Inicializa DB antes de qualquer subcomando que use o banco.
    tauri::async_runtime::block_on(async {
        runner::init_database(&handle).await.expect("falha ao iniciar o banco");
    });

    match cli.command.unwrap_or(Command::ListDevices) {
        Command::ListDevices => {
            println!("(list-devices: implementado na Task 2)");
        }
        Command::ListModels => {
            println!("(list-models: implementado na Task 2)");
        }
        Command::Record(_args) => {
            println!("(record: implementado na Task 3)");
        }
    }
}
```

- [ ] **Step 6: Compilar e rodar o esqueleto**

Run: `cargo build --bin meetily-cli`
Expected: compila. Se falhar por janela obrigatória no contexto Tauri, aplicar a nota do Step 4.

Run: `cargo run --bin meetily-cli -- list-devices`
Expected: imprime a linha placeholder e encerra **sem abrir nenhuma janela** e sem panic. Confirma que o app headless sobe e o DB inicializa (verificar no log "Database" / arquivo `meeting_minutes.sqlite` no app_data_dir).

- [ ] **Step 7: Commit**

```bash
git add frontend/src-tauri/Cargo.toml frontend/src-tauri/src/lib.rs frontend/src-tauri/src/cli frontend/src-tauri/src/bin/meetily_cli.rs
git commit -m "feat(cli): esqueleto do meetily-cli headless (args + app Tauri sem janela + init DB)"
```

---

## Task 2: Subcomandos `list-devices` e `list-models`

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`
- Modify: `frontend/src-tauri/src/bin/meetily_cli.rs`

- [ ] **Step 1: Investigar as funções exatas a reusar**

Run: `grep -nE "pub async fn list_audio_devices|pub fn list_audio_devices|pub async fn whisper_get_available_models|pub async fn parakeet_get_available_models|pub async fn api_get_transcript_config|struct ModelInfo" frontend/src-tauri/src/audio/devices/discovery.rs frontend/src-tauri/src/whisper_engine/commands.rs frontend/src-tauri/src/parakeet_engine/commands.rs frontend/src-tauri/src/api/api.rs`

Decisão: usar `list_audio_devices` (discovery) para dispositivos; `whisper_get_available_models()` + `parakeet_get_available_models()` para modelos; `api_get_transcript_config(app, state)` para descobrir o default (`provider`/`model`). Anotar as assinaturas reais e ajustar as chamadas abaixo se diferirem.

- [ ] **Step 2: Implementar no runner**

Adicionar em `runner.rs` (ajustar nomes conforme Step 1):

```rust
pub async fn run_list_devices() -> Result<(), String> {
    let devices = crate::audio::devices::discovery::list_audio_devices()
        .map_err(|e| e.to_string())?;
    println!("Dispositivos de áudio:");
    for d in devices {
        println!("  - {}", d); // ajustar campo conforme o tipo retornado
    }
    Ok(())
}

pub async fn run_list_models(app: &tauri::AppHandle) -> Result<(), String> {
    let default = crate::api::api::api_get_transcript_config(
        app.clone(),
        app.state::<crate::state::AppState>(),
        None,
    )
    .await
    .ok(); // (engine, model) default

    let whisper = crate::whisper_engine::commands::whisper_get_available_models()
        .await
        .unwrap_or_default();
    let parakeet = crate::parakeet_engine::commands::parakeet_get_available_models()
        .await
        .unwrap_or_default();

    println!("Padrão: {:?}", default);
    println!("Whisper:");
    for m in whisper { println!("  - {}", m.name); }   // ajustar campo de ModelInfo
    println!("Parakeet:");
    for m in parakeet { println!("  - {}", m.name); }
    Ok(())
}
```

> Os diretórios de modelo são resolvidos via `set_models_directory(&app)` (whisper/parakeet),
> que usam `AppHandle`. Chamar ambos logo após `init_database` no bin (Step 3).

- [ ] **Step 3: Ligar no bin**

Em `meetily_cli.rs`, após `init_database`, antes do match, inicializar os diretórios de modelo:

```rust
tauri::async_runtime::block_on(async {
    runner::init_database(&handle).await.expect("falha ao iniciar o banco");
});
app_lib::whisper_engine::commands::set_models_directory(&handle);
app_lib::parakeet_engine::commands::set_models_directory(&handle);
```

E nos braços do match:

```rust
Command::ListDevices => {
    tauri::async_runtime::block_on(runner::run_list_devices())
        .unwrap_or_else(|e| eprintln!("erro: {e}"));
}
Command::ListModels => {
    tauri::async_runtime::block_on(runner::run_list_models(&handle))
        .unwrap_or_else(|e| eprintln!("erro: {e}"));
}
```

- [ ] **Step 4: Rodar**

Run: `cargo run --bin meetily-cli -- list-devices`
Expected: lista o microfone e o loopback do sistema reais.

Run: `cargo run --bin meetily-cli -- list-models`
Expected: lista modelos whisper/parakeet presentes na pasta de modelos do app e indica o padrão.

- [ ] **Step 5: Commit**

```bash
git add frontend/src-tauri/src/cli/runner.rs frontend/src-tauri/src/bin/meetily_cli.rs
git commit -m "feat(cli): subcomandos list-devices e list-models"
```

---

## Task 3: Gravação normal (com IA) + transcrição ao vivo no terminal + salvar no DB

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`
- Modify: `frontend/src-tauri/src/bin/meetily_cli.rs`

- [ ] **Step 1: Investigar a persistência no DB pós-stop**

Run: `grep -nE "save_transcript|TranscriptsRepository|folder_path|recording folder|current_meeting_folder|get_meeting_folder" frontend/src-tauri/src/audio/recording_saver.rs frontend/src-tauri/src/audio/recording_manager.rs frontend/src-tauri/src/database/repositories/*.rs`

Decisão a anotar:
1. Onde obter o **folder_path** da gravação que acabou (método no `RecordingManager`/saver, ou via `RECORDING_MANAGER`).
2. A assinatura de `TranscriptsRepository::save_transcript(pool, &title, &segments, folder_path)` (vista em `api.rs:980`).
3. Se a forma mais simples é **ler o `transcripts.json`** recém-escrito da pasta e mapear para `Vec<TranscriptSegment>` antes de salvar. (Recomendado: reusar o que `api_save_transcript` faz, chamando o repositório direto.)

- [ ] **Step 2: Implementar `run_record` (modo normal)**

Em `runner.rs`:

```rust
use crate::cli::args::RecordArgs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Listener, Manager};

pub async fn run_record(app: &tauri::AppHandle, args: RecordArgs) -> Result<(), String> {
    args.validate()?;

    // Garante motor/modelo inicializados (espelha lib.rs setup).
    if let Err(e) = crate::whisper_engine::commands::whisper_init().await {
        log::warn!("whisper_init: {e}");
    }
    if let Err(e) = crate::parakeet_engine::commands::parakeet_init().await {
        log::warn!("parakeet_init: {e}");
    }

    // Listener de transcrição → imprime no terminal.
    let quiet = args.quiet;
    app.listen("transcript-update", move |event: tauri::Event| {
        if quiet { return; }
        if let Ok(u) = serde_json::from_str::<crate::audio::recording_commands::TranscriptUpdate>(event.payload()) {
            println!("[{}] {}", u.timestamp, u.text);
        }
    });

    // Inicia a gravação (reusa todo o pipeline do app: VAD + mixagem + transcrição).
    crate::audio::recording_commands::start_recording_with_meeting_name(
        app.clone(),
        args.name.clone(),
    )
    .await?;

    println!("Gravando. Ctrl+C para parar e salvar.");

    // Espera Ctrl+C ou --duration.
    wait_for_stop(args.duration).await;

    // Para e persiste arquivos (transcripts.json/metadata/audio).
    crate::audio::recording_commands::stop_recording(
        app.clone(),
        crate::audio::recording_commands::RecordingArgs::default(),
    )
    .await?;

    // Persiste no DB (espelha o que o frontend faz após o stop).
    persist_to_db(app).await?;

    Ok(())
}

async fn wait_for_stop(duration: Option<u64>) {
    let ctrl_c = tokio::signal::ctrl_c();
    match duration {
        Some(secs) => {
            tokio::select! {
                _ = ctrl_c => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {},
            }
        }
        None => { let _ = ctrl_c.await; }
    }
}
```

> `RecordingArgs::default()` exige que o struct derive `Default` (verificar em
> recording_commands.rs:56; se não derivar, construir o struct com os campos vistos).

- [ ] **Step 3: Implementar `persist_to_db` (conforme decisão do Step 1)**

Em `runner.rs` (ajustar à API real encontrada no Step 1):

```rust
async fn persist_to_db(app: &tauri::AppHandle) -> Result<(), String> {
    // 1. Descobrir título + folder_path da gravação que acabou (Step 1, item 1).
    // 2. Ler transcripts.json da pasta e mapear para Vec<TranscriptSegment>.
    // 3. Chamar o repositório:
    let state = app.state::<crate::state::AppState>();
    let pool = state.db_manager.pool();
    let meeting_id = crate::database::repositories::transcripts::TranscriptsRepository::save_transcript(
        pool, &title, &segments, Some(folder_path.clone()),
    )
    .await
    .map_err(|e| format!("Falha ao salvar no DB: {}", e))?;
    println!("✓ Salvo. meeting_id={meeting_id}  pasta={folder_path}");
    Ok(())
}
```

> Caminho do módulo do repositório a confirmar no Step 1 (o `use` em api.rs revela o path exato).

- [ ] **Step 4: Ligar no bin**

```rust
Command::Record(args) => {
    tauri::async_runtime::block_on(runner::run_record(&handle, args))
        .unwrap_or_else(|e| { eprintln!("erro: {e}"); std::process::exit(1); });
}
```

- [ ] **Step 5: Verificação ponta a ponta (rodando)**

Run: `cargo run --bin meetily-cli -- record --name "CLI E2E"`
Fale algo no microfone por ~15s, depois Ctrl+C.
Expected:
- Linhas `[hh:mm:ss] texto` aparecem ao vivo enquanto você fala.
- No fim, imprime `✓ Salvo. meeting_id=...`.
- Abrir o app Meetily → a reunião "CLI E2E" aparece na lista com a transcrição e o áudio.

- [ ] **Step 6: Commit**

```bash
git add frontend/src-tauri/src/cli/runner.rs frontend/src-tauri/src/bin/meetily_cli.rs
git commit -m "feat(cli): gravacao normal com transcricao ao vivo no terminal e persistencia no DB"
```

---

## Task 4: Painel vivo (VU + ponto piscando + contador + alerta de silêncio)

**Files:**
- Create/fill: `frontend/src-tauri/src/cli/panel.rs`
- Modify: `frontend/src-tauri/src/cli/runner.rs`
- Modify: `frontend/src-tauri/Cargo.toml` (se optar por `crossterm`)

- [ ] **Step 1: Decisão de terminal**

Default: **ANSI manual + `\r`** (sem nova dependência) para a linha de status fixa. Só adicionar `crossterm` se o controle de cursor manual no PowerShell se mostrar problemático no Step 5. Registrar a decisão num comentário no topo de `panel.rs`.

- [ ] **Step 2: Render puro do painel, com teste**

Fill `frontend/src-tauri/src/cli/panel.rs`:

```rust
/// Estado renderizável do painel (alimentado por audio-levels + contadores).
#[derive(Debug, Clone, Default)]
pub struct PanelState {
    pub elapsed_secs: u64,
    pub mic_rms: f32,
    pub system_rms: f32,
    pub bands: [f32; 3],   // graves, médios, agudos (0.0..=1.0) — preenchido na Task 5
    pub segments: u64,
    pub engine_model: String,
    pub record_only: bool,
    pub bytes_written: u64,
    pub silent_secs: u64,  // segundos com RMS ~0
    pub pulse_on: bool,    // alterna a cada segundo
}

fn bar(level: f32) -> char {
    let blocks = ['▁','▂','▃','▄','▅','▆','▇','█'];
    let idx = (level.clamp(0.0, 1.0) * (blocks.len() as f32 - 1.0)).round() as usize;
    blocks[idx]
}

fn meter(level: f32, width: usize) -> String {
    let filled = (level.clamp(0.0, 1.0) * width as f32).round() as usize;
    let mut s = String::new();
    for i in 0..width { s.push(if i < filled { '▮' } else { '▯' }); }
    s
}

fn hhmmss(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Renderiza UMA linha de status (sem newline). O chamador faz `\r` + flush.
pub fn render(s: &PanelState) -> String {
    let dot = if s.pulse_on { '●' } else { '○' };
    let tail = if s.record_only {
        format!("gravado: {:.1} MB / {}",
            s.bytes_written as f64 / 1_048_576.0, hhmmss(s.elapsed_secs))
    } else {
        format!("trechos:{}", s.segments)
    };
    let warn = if s.silent_secs >= 5 {
        format!("  ⚠ sem áudio há {}s", s.silent_secs)
    } else { String::new() };
    format!(
        "{dot} REC {time}  │ graves {g} médios {m} agudos {a} │ mic {mic}  sis {sys} │ {tail} │ {em}{warn}",
        time = hhmmss(s.elapsed_secs),
        g = bar(s.bands[0]), m = bar(s.bands[1]), a = bar(s.bands[2]),
        mic = meter(s.mic_rms, 4), sys = meter(s.system_rms, 4),
        tail = tail, em = s.engine_model, warn = warn,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_only_shows_bytes_not_segments() {
        let s = PanelState { record_only: true, bytes_written: 2_097_152,
            elapsed_secs: 65, ..Default::default() };
        let out = render(&s);
        assert!(out.contains("gravado: 2.0 MB / 00:01:05"));
        assert!(!out.contains("trechos:"));
    }

    #[test]
    fn silence_warning_after_5s() {
        let s = PanelState { silent_secs: 6, ..Default::default() };
        assert!(render(&s).contains("⚠ sem áudio há 6s"));
    }

    #[test]
    fn pulse_toggles_dot() {
        let on = PanelState { pulse_on: true, ..Default::default() };
        let off = PanelState { pulse_on: false, ..Default::default() };
        assert!(render(&on).starts_with('●'));
        assert!(render(&off).starts_with('○'));
    }
}
```

- [ ] **Step 3: Rodar os testes**

Run: `cargo test --lib cli::panel`
Expected: PASS nos três testes.

- [ ] **Step 4: Wiring — consumir `audio-levels` e desenhar a linha**

Em `runner.rs`, dentro de `run_record`, antes de `wait_for_stop`, manter um `Arc<Mutex<PanelState>>` atualizado pelo listener `audio-levels` e por um contador de `transcript-update`, e uma task que redesenha a linha ~4×/s:

```rust
use std::sync::Mutex;
let panel = Arc::new(Mutex::new(crate::cli::panel::PanelState {
    engine_model: engine_model_label.clone(), // resolvido de transcript-config
    record_only: args.record_only,
    ..Default::default()
}));

// audio-levels → atualiza RMS + silêncio
{
    let panel = panel.clone();
    app.listen("audio-levels", move |e: tauri::Event| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
            let mut p = panel.lock().unwrap();
            p.mic_rms = v["mic_rms"].as_f64().unwrap_or(0.0) as f32;
            p.system_rms = v["system_rms"].as_f64().unwrap_or(0.0) as f32;
            if let Some(b) = v["bands"].as_array() { // presente após Task 5
                for i in 0..3 { p.bands[i] = b.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32; }
            }
        }
    });
}

// contador de trechos (somar no MESMO listener de transcript-update da Task 3)

// task de desenho
let start = std::time::Instant::now();
{
    let panel = panel.clone();
    tokio::spawn(async move {
        use std::io::Write;
        let mut tick = 0u64;
        let mut iv = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            iv.tick().await;
            tick += 1;
            let mut p = panel.lock().unwrap();
            p.elapsed_secs = start.elapsed().as_secs();
            p.pulse_on = (p.elapsed_secs % 2) == 0;
            if p.mic_rms < 0.01 && p.system_rms < 0.01 { /* incrementar silent a cada ~1s */ }
            let line = crate::cli::panel::render(&p);
            drop(p);
            print!("\r\x1b[K{}", line);
            let _ = std::io::stdout().flush();
        }
    });
}
```

> No modo normal, as linhas de transcrição (`println!`) "empurram" o painel; aceitável na v1.
> Para não embaralhar, ao imprimir uma fala, primeiro emitir `\r\x1b[K` (limpa a linha do painel),
> dar `println!` da fala, e deixar a task redesenhar o painel no próximo tick.

- [ ] **Step 5: Verificação visual (rodando)**

Run: `cargo run --bin meetily-cli -- record --name "Painel"`
Expected: linha de status com `●/○` piscando ~1Hz, VU de mic/sistema reagindo à voz, contador de trechos subindo; ficar 6s em silêncio → aparece `⚠ sem áudio`. Validar legibilidade no Windows PowerShell; se o cursor bagunçar, adotar `crossterm` (Step 1) e reimplementar o desenho com `crossterm::cursor`/`queue!`.

- [ ] **Step 6: Commit**

```bash
git add frontend/src-tauri/src/cli/panel.rs frontend/src-tauri/src/cli/runner.rs frontend/src-tauri/Cargo.toml
git commit -m "feat(cli): painel vivo com VU, ponto piscante, contador e alerta de silencio"
```

---

## Task 5: Equalizador FFT de 3 bandas

**Files:**
- Modify: `frontend/src-tauri/src/audio/recording_state.rs` (expor janela de amostras, se necessário)
- Modify: `frontend/src-tauri/src/audio/recording_commands.rs` (incluir `bands` no payload)
- Create: `frontend/src-tauri/src/cli/fft.rs` (mapeamento de bandas, puro + testável) — adicionar `pub mod fft;` em `cli/mod.rs`

- [ ] **Step 1: Localizar o código de FFT de "5 barras" citado pelo usuário**

Perguntar ao usuário onde está, e/ou:
Run: `grep -rniE "fft|band|spectrum|barra|equaliz|magnitude|freq" frontend/src-tauri/src frontend/src --include=*.rs --include=*.ts --include=*.tsx | grep -viE "audio_processing.rs|noise" | head -40`

Decisão: se existir computação de bandas reutilizável (Rust), reusar suas fronteiras de banda e só reduzir 5→3. Se for só visual no front (JS), portar apenas as fronteiras de frequência.

- [ ] **Step 2: Verificar de onde vêm as amostras**

Run: `grep -nE "fn mic_rms|fn system_rms|VecDeque|samples|ring|recent|window|pub fn .*sample" frontend/src-tauri/src/audio/recording_state.rs`

Decisão:
- Se `RecordingState` já guarda uma janela recente de amostras do mix, usar.
- Senão, adicionar um buffer curto (ex.: últimos 1024 samples do mix) alimentado no mesmo ponto onde `mic_rms`/`system_rms` são atualizados, e um getter `recent_mix_window(&self) -> Vec<f32>`.

- [ ] **Step 3: Função pura de bandas, com teste**

Create `frontend/src-tauri/src/cli/fft.rs`:

```rust
use realfft::RealFftPlanner;

/// Calcula 3 magnitudes de banda (graves/médios/agudos), normalizadas 0..=1,
/// a partir de uma janela de amostras mono e da sample rate.
pub fn three_bands(samples: &[f32], sample_rate: u32) -> [f32; 3] {
    let n = samples.len();
    if n < 16 { return [0.0; 3]; }
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(n);
    let mut input = samples.to_vec();
    let mut spectrum = r2c.make_output_vec();
    if r2c.process(&mut input, &mut spectrum).is_err() { return [0.0; 3]; }

    let bin_hz = sample_rate as f32 / n as f32;
    // Fronteiras: graves <250Hz, médios 250–2000Hz, agudos >2000Hz.
    let mut bands = [0.0f32; 3];
    let mut counts = [0u32; 3];
    for (i, c) in spectrum.iter().enumerate() {
        let f = i as f32 * bin_hz;
        let mag = c.norm();
        let b = if f < 250.0 { 0 } else if f < 2000.0 { 1 } else { 2 };
        bands[b] += mag;
        counts[b] += 1;
    }
    for i in 0..3 {
        if counts[i] > 0 { bands[i] /= counts[i] as f32; }
        // normalização log simples p/ caber em 0..=1 (ajustar fator no Step 6)
        bands[i] = (bands[i].max(0.0).ln_1p() / 6.0).clamp(0.0, 1.0);
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq: f32, sr: u32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * freq * i as f32 / sr as f32).sin()).collect()
    }

    #[test]
    fn low_tone_dominates_bass_band() {
        let s = sine(100.0, 16000, 1024);
        let b = three_bands(&s, 16000);
        assert!(b[0] > b[1] && b[0] > b[2], "graves deveriam dominar: {b:?}");
    }

    #[test]
    fn high_tone_dominates_treble_band() {
        let s = sine(6000.0, 16000, 1024);
        let b = three_bands(&s, 16000);
        assert!(b[2] > b[0], "agudos deveriam dominar graves: {b:?}");
    }

    #[test]
    fn silence_is_zero() {
        let b = three_bands(&vec![0.0; 1024], 16000);
        assert_eq!(b, [0.0, 0.0, 0.0]);
    }
}
```

Add `pub mod fft;` em `cli/mod.rs`.

- [ ] **Step 4: Rodar testes**

Run: `cargo test --lib cli::fft`
Expected: PASS (graves/agudos/silêncio).

- [ ] **Step 5: Incluir `bands` no payload `audio-levels`**

Em `recording_commands.rs`, no loop de emissão (linhas ~321-327), calcular as bandas a partir da janela do mix e adicionar ao JSON:

```rust
let mic_rms = recording_state_arc.mic_rms();
let sys_rms = recording_state_arc.system_rms();
let window = recording_state_arc.recent_mix_window(); // getter da Step 2
let bands = crate::cli::fft::three_bands(&window, 16000); // sr do pipeline; confirmar

let update = serde_json::json!({
    "mic_rms": mic_rms,
    "system_rms": sys_rms,
    "bands": [bands[0], bands[1], bands[2]],
});
```

> A sample rate do pipeline: confirmar (CLAUDE.md menciona 48kHz na captura; o VAD/whisper
> usa 16kHz). Usar a sr da janela que estiver disponível em `RecordingState`.

- [ ] **Step 6: Verificação visual + calibração**

Run: `cargo run --bin meetily-cli -- record --name "FFT"`
Expected: as 3 barras (graves/médios/agudos) reagem ao espectro da voz/música. Ajustar o fator de normalização (`/ 6.0` no Step 3) para que fala normal preencha ~50–80% sem saturar.

- [ ] **Step 7: Commit**

```bash
git add frontend/src-tauri/src/cli/fft.rs frontend/src-tauri/src/cli/mod.rs frontend/src-tauri/src/audio/recording_commands.rs frontend/src-tauri/src/audio/recording_state.rs
git commit -m "feat(cli): equalizador FFT de 3 bandas no painel (DSP no Rust, consumido pela CLI)"
```

---

## Task 6: Modo `--record-only` (sem IA) + reunião re-transcritível

**Files:**
- Modify: `frontend/src-tauri/src/audio/recording_commands.rs` (novo `start_recording_only`)
- Modify: `frontend/src-tauri/src/cli/runner.rs`

- [ ] **Step 1: Criar `start_recording_only` (espelha o normal, sem validação de modelo e sem transcrição)**

Em `recording_commands.rs`, criar uma função baseada em `start_recording_with_meeting_name`
(linhas 130-394), porém:
- **remover** a validação `validate_transcription_model_ready` (linhas 146-161);
- após `manager.start_recording(...)` (linha 295), **descartar** o `transcription_receiver`
  (não chamar `transcription::start_transcription_task`, linhas 340-345);
- **não** registrar o listener de `transcript-update` (linhas 347-380) — não há transcrição;
- **manter** o loop de emissão `audio-levels` (linhas 314-338), pois o painel depende dele.

```rust
pub async fn start_recording_only<R: Runtime>(
    app: AppHandle<R>,
    meeting_name: Option<String>,
) -> Result<(), String> {
    if IS_RECORDING.load(Ordering::SeqCst) {
        return Err("Recording already in progress".to_string());
    }
    let mut manager = RecordingManager::new();
    // ... (copiar do normal: preferências, devices, auto_save=true FORÇADO) ...
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        format!("Meeting {}", chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"))
    });
    manager.set_meeting_name(Some(effective_meeting_name));
    // engine/model vazios: gravação sem IA
    manager.set_transcription_info(None, None);

    let _receiver = manager
        .start_recording(microphone_device, system_device, /*auto_save=*/ true, recording_mode)
        .await
        .map_err(|e| format!("Failed to start recording: {}", e))?;
    let recording_state_arc = manager.recording_state();
    { *RECORDING_MANAGER.lock().unwrap() = Some(manager); }
    IS_RECORDING.store(true, Ordering::SeqCst);
    // copiar o bloco de emissão de audio-levels (314-338) aqui
    Ok(())
}
```

> `auto_save=true` é obrigatório: o `.mp4` é a fonte da re-transcrição. A flag `--no-audio-save`
> já é rejeitada na validação (Task 1).

- [ ] **Step 2: Garantir a criação da reunião no DB com transcrição vazia**

No `runner.rs`, `run_record`: quando `args.record_only`, após `stop_recording`, chamar
`persist_to_db` com `segments` vazio (a reunião precisa existir no DB para o app oferecer
re-transcrição). Confirmar (Step 1 da Task 3) que `save_transcript` cria a reunião mesmo com
`segments` vazio; se não criar, usar o repositório de meetings para inserir a linha + folder_path.

```rust
if args.record_only {
    crate::audio::recording_commands::start_recording_only(app.clone(), args.name.clone()).await?;
} else {
    // caminho da Task 3
}
```

E no painel, `record_only=true` faz o tail virar "gravado: X MB / tempo" (já implementado na Task 4).
Atualizar `bytes_written` a partir do tamanho do arquivo de áudio em escrita (ou de um contador do saver).

- [ ] **Step 3: Verificação ponta a ponta (rodando)**

Run: `cargo run --bin meetily-cli -- record --record-only --name "SemIA"`
Fale ~15s, Ctrl+C.
Expected:
- **Nenhum** modelo de IA carregado (verificar no log que `whisper_init`/transcrição NÃO rodaram; RAM menor).
- Painel mostra "gravado: X MB / tempo" subindo, VU e FFT funcionando.
- No app: a reunião "SemIA" aparece com transcrição vazia; clicar **Re-transcrever** processa o áudio salvo e preenche os trechos.

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/src/audio/recording_commands.rs frontend/src-tauri/src/cli/runner.rs
git commit -m "feat(cli): modo --record-only (grava sem IA, re-transcrevivel pelo app)"
```

---

## Task 7: Polimento — `--engine/--model`, `--partial`, banner, erros, ajuda

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`

- [ ] **Step 1: Override de engine/model**

Antes de iniciar a gravação no `run_record`, se `args.engine`/`args.model` vierem preenchidos,
aplicar via `api_save_transcript_config` (ou carregar o modelo via `whisper_load_model`/
`parakeet_load_model`) para que o worker use o escolhido. Confirmar a assinatura em
`api.rs:654` (`api_save_transcript_config`) e nos `*_load_model`.

```rust
if let Some(engine) = &args.engine {
    // ajustar provider na config; depois carregar o modelo (args.model) se informado
}
```

- [ ] **Step 2: `--partial` (mostrar parciais ao vivo)**

Adicionar um listener em `transcript-partial` (evento já emitido, worker.rs:229) que,
quando `args.partial`, sobrescreve a linha atual com o texto parcial (cinza, via `\r`).
Os finais (`transcript-update`) continuam imprimindo a linha definitiva.

```rust
if args.partial {
    app.listen("transcript-partial", move |e| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
            if let Some(t) = v["text"].as_str() {
                print!("\r\x1b[K\x1b[90m… {}\x1b[0m", t);
                let _ = std::io::stdout().flush();
            }
        }
    });
}
```

- [ ] **Step 3: Banner de início**

No começo de `run_record`, após resolver engine/modelo/dispositivos, imprimir:

```rust
println!("✓ Motor: {engine_model}   ✓ Mic: {mic}   ✓ Sistema: {sys}");
println!("✓ Reunião: \"{name}\"  → {folder}");
println!("Gravando. Ctrl+C para parar e salvar.");
```

(O `folder` pode ser obtido após `start_recording*` via o método identificado na Task 3/Step 1.)

- [ ] **Step 4: Erros amigáveis**

- Falha de dispositivo de áudio → mensagem clara + `std::process::exit(1)` (já no bin).
- Modo normal com modelo não baixado → `start_recording_with_meeting_name` já retorna erro
  de validação; capturar e orientar: "Modelo não disponível. Baixe pelo app ou use --record-only."

```rust
if let Err(e) = start_result {
    if e.contains("model") || e.contains("download") {
        eprintln!("Modelo não disponível. Baixe pelo app, ou grave sem IA com --record-only.");
    }
    return Err(e);
}
```

- [ ] **Step 5: Verificação**

Run: `cargo run --bin meetily-cli -- record --help`
Expected: ajuda do clap lista todas as flags.

Run: `cargo run --bin meetily-cli -- record --engine whisper --model base.en --partial --name "Polish"`
Expected: banner correto; parciais aparecem em cinza e viram finais; salva no DB.

Run (erro): renomear/remover temporariamente o modelo e rodar sem `--record-only`.
Expected: mensagem orientando baixar pelo app ou usar `--record-only`; exit code 1.

- [ ] **Step 6: Commit**

```bash
git add frontend/src-tauri/src/cli/runner.rs
git commit -m "feat(cli): overrides de engine/model, --partial, banner de inicio e erros amigaveis"
```

---

## Notas finais

- **Fora de escopo (v1):** download de modelos, geração de sumário (LLM), re-transcrição via CLI.
- **Riscos de integração conhecidos (verificar cedo):**
  1. Build do app Tauri headless sem janela (Task 1, Step 6) — maior incógnita; resolver antes de avançar.
  2. Obtenção do `folder_path` da gravação e criação da reunião no DB com transcrição vazia (Tasks 3 e 6).
  3. Fonte/sample-rate das amostras para o FFT (Task 5, Steps 2 e 5).
- **Documentação:** ao final, adicionar uma seção sobre o `meetily-cli` em `docs/` (uso, flags, modo record-only) — fora dos commits de código acima, opcional.
