# CLI Summarize Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Adicionar à `meetily-cli` os comandos `summarize`, `list-meetings`, `list-templates` e a flag `record --summarize`, reusando o pipeline de sumário do app.

**Architecture:** A CLI espelha `api_process_transcript` mas **aguarda** (`.await`) `SummaryService::process_transcript_background` em vez de `spawn`, depois lê o status persistido para confirmar. Provider/model vêm sempre da config do app (`settings`). Saída: só salva (banco + `summary.md`), sem imprimir markdown.

**Tech Stack:** Rust, clap, tauri (headless), sqlx, tokio.

## Global Constraints

- **NÃO rodar `cargo check`/`build`/`test` em `frontend/src-tauri`.** O usuário compila e testa localmente. Os passos "verify" deste plano são executados PELO USUÁRIO, não pelo agente.
- **Um único commit no FINAL** (Task 8). Nenhuma task intermediária commita.
- Default de template: `"daily_standup"` (idêntico a `api_process_transcript`).
- Defaults de chunk: `chunk_size = 40000`, `overlap = 1000` (idênticos a `api_process_transcript`).
- Provider/model do sumário: SEMPRE de `SettingsRepository::get_model_config` (tabela `settings`). Sem overrides por flag.
- Saída do comando: só persiste (banco + `summary.md` via reuso do pipeline). Não imprime o markdown.

---

### Task 1: args.rs — comandos, args e validação

**Files:**
- Modify: `frontend/src-tauri/src/cli/args.rs`

**Interfaces:**
- Produces: `Command::Summarize(SummarizeArgs)`, `Command::ListMeetings`, `Command::ListTemplates`; `SummarizeArgs { meeting: Option<String>, last: bool, template: Option<String> }` com `validate()`; `RecordArgs` ganha `summarize: bool` e `template: Option<String>`.

- [ ] **Step 1: Adicionar variantes ao enum `Command`**

Em `frontend/src-tauri/src/cli/args.rs`, no `pub enum Command`, depois de `ListDevices,`:

```rust
    /// List meetings (id, date, title) to discover meeting ids
    ListMeetings,
    /// List available summary templates (id, name, description)
    ListTemplates,
    /// Generate the summary of a meeting (reuses the app's summary pipeline)
    Summarize(SummarizeArgs),
```

- [ ] **Step 2: Adicionar os campos novos em `RecordArgs`**

No `pub struct RecordArgs`, depois do campo `no_audio_save`:

```rust
    /// After saving, generate the summary of the just-recorded meeting
    #[arg(long)]
    pub summarize: bool,
    /// Template id used when --summarize is set (default: "daily_standup")
    #[arg(long)]
    pub template: Option<String>,
```

- [ ] **Step 3: Adicionar a regra em `RecordArgs::validate`**

Dentro de `impl RecordArgs { pub fn validate(...)`, antes de `Ok(())`:

```rust
        if self.summarize && self.record_only {
            return Err(
                "--summarize needs a transcription; it is incompatible with --record-only."
                    .to_string(),
            );
        }
```

- [ ] **Step 4: Definir `SummarizeArgs` e seu `validate`**

Depois do `impl RecordArgs { ... }` (antes do `#[cfg(test)] mod tests`):

```rust
#[derive(clap::Args, Debug, Default, Clone)]
pub struct SummarizeArgs {
    /// Meeting id to summarize (use `list-meetings` to discover ids)
    #[arg(long)]
    pub meeting: Option<String>,
    /// Summarize the most recent meeting instead of passing an id
    #[arg(long)]
    pub last: bool,
    /// Template id (default: "daily_standup"); see `list-templates`
    #[arg(long)]
    pub template: Option<String>,
}

impl SummarizeArgs {
    pub fn validate(&self) -> Result<(), String> {
        match (self.meeting.is_some(), self.last) {
            (true, true) => {
                Err("use either --meeting <id> or --last, not both.".to_string())
            }
            (false, false) => {
                Err("specify the meeting to summarize: --meeting <id> or --last.".to_string())
            }
            _ => Ok(()),
        }
    }
}
```

- [ ] **Step 5: Adicionar os testes**

Dentro de `mod tests`, depois do último teste existente:

```rust
    #[test]
    fn summarize_requires_a_target() {
        let cli = Cli::parse_from(["meetily-cli", "summarize"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn summarize_rejects_both_targets() {
        let cli = Cli::parse_from(["meetily-cli", "summarize", "--meeting", "x", "--last"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn summarize_last_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "summarize", "--last"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_ok());
        assert!(args.last);
    }

    #[test]
    fn summarize_meeting_id_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "summarize", "--meeting", "abc"]);
        let Some(Command::Summarize(args)) = cli.command else { panic!("expected Summarize") };
        assert!(args.validate().is_ok());
        assert_eq!(args.meeting.as_deref(), Some("abc"));
    }

    #[test]
    fn record_summarize_with_record_only_is_rejected() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--summarize", "--record-only"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_err());
    }

    #[test]
    fn record_summarize_alone_is_ok() {
        let cli = Cli::parse_from(["meetily-cli", "record", "--summarize"]);
        let Some(Command::Record(args)) = cli.command else { panic!("expected Record") };
        assert!(args.validate().is_ok());
        assert!(args.summarize);
    }
```

- [ ] **Step 6: (USUÁRIO) compilar e rodar os testes localmente**

Run (pelo usuário): `cargo test -p app_lib cli::args`
Expected: todos os testes de `cli::args` passam.

---

### Task 2: runner.rs — `run_list_meetings` e `run_list_templates`

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`

**Interfaces:**
- Consumes: `MeetingsRepository::get_meetings`, `crate::summary::templates::list_templates`.
- Produces: `pub async fn run_list_meetings(app: &tauri::AppHandle) -> Result<(), String>`, `pub async fn run_list_templates() -> Result<(), String>`.

- [ ] **Step 1: Adicionar `run_list_meetings`**

No fim de `frontend/src-tauri/src/cli/runner.rs` (antes de `fn dir_size_bytes`):

```rust
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
```

- [ ] **Step 2: Adicionar `run_list_templates`**

Logo abaixo de `run_list_meetings`:

```rust
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
```

- [ ] **Step 3: (USUÁRIO) compilar localmente**

Run (pelo usuário): `cargo check -p app_lib`
Expected: compila sem erros.

---

### Task 3: runner.rs — `summarize_meeting` (spinner + pipeline)

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`

**Interfaces:**
- Consumes: `SummaryProcessesRepository::{create_or_reset_process, get_summary_data}`, `TranscriptChunksRepository::save_transcript_data`, `SummaryService::process_transcript_background`.
- Produces: `pub async fn summarize_meeting(app: &tauri::AppHandle, meeting_id: &str, text: String, provider: String, model: String, template_id: String) -> Result<(), String>`.

- [ ] **Step 1: Adicionar `summarize_meeting`**

No fim de `runner.rs` (antes de `fn dir_size_bytes`):

```rust
/// Núcleo da geração de sumário pela CLI: espelha `api_process_transcript`, mas
/// AGUARDA o processamento (em vez de `spawn`). Mostra um spinner com tempo
/// decorrido enquanto o LLM trabalha e, ao final, lê o status persistido para
/// confirmar (`completed`) ou propagar o erro do banco.
///
/// `provider`/`model` são os da config do app (settings). `text` é o transcript
/// concatenado. Persiste no banco e exporta `summary.md` via reuso do pipeline.
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
```

- [ ] **Step 2: Confirmar imports já disponíveis no topo de `runner.rs`**

Verificar que `runner.rs` já tem (são usados acima):
- `use std::io::Write;` (linha ~4)
- `use tauri::Manager;` (linha ~6, fornece `.state()`)
- `use crate::state::AppState;` (linha ~3)

Se algum faltar, adicionar. Nenhuma alteração esperada (já presentes).

- [ ] **Step 3: (USUÁRIO) compilar localmente**

Run (pelo usuário): `cargo check -p app_lib`
Expected: compila sem erros. (Atenção a assinatura de `save_transcript_data`: `(pool, meeting_id, text, model, model_name, chunk_size, overlap)` — aqui `model = provider`, `model_name = model`, como em `api_process_transcript`.)

---

### Task 4: runner.rs — `summarize_existing_meeting` (helper DRY)

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`

**Interfaces:**
- Consumes: `MeetingsRepository::get_meeting`, `SettingsRepository::get_model_config`, `crate::summary::templates::{get_template, list_templates}`, `summarize_meeting` (Task 3).
- Produces: `async fn summarize_existing_meeting(app: &tauri::AppHandle, meeting_id: &str, template: Option<String>) -> Result<(), String>`.

- [ ] **Step 1: Adicionar `summarize_existing_meeting`**

No fim de `runner.rs` (antes de `fn dir_size_bytes`):

```rust
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

    // Transcript concatenado.
    let details = MeetingsRepository::get_meeting(&pool, meeting_id)
        .await
        .map_err(|e| format!("Failed to load the meeting: {}", e))?
        .ok_or_else(|| format!("Meeting {} not found.", meeting_id))?;
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
```

- [ ] **Step 2: (USUÁRIO) compilar localmente**

Run (pelo usuário): `cargo check -p app_lib`
Expected: compila sem erros. (`Setting` tem `provider: String` e `model: String`; `MeetingDetails.transcripts` é `Vec<MeetingTranscript>` com campo `text`.)

---

### Task 5: runner.rs — `run_summarize`

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs`

**Interfaces:**
- Consumes: `RecordArgs`-style import já existe; precisa `SummarizeArgs`; `MeetingsRepository::get_meetings`; `summarize_existing_meeting` (Task 4).
- Produces: `pub async fn run_summarize(app: &tauri::AppHandle, args: SummarizeArgs) -> Result<(), String>`.

- [ ] **Step 1: Importar `SummarizeArgs`**

No topo de `runner.rs`, a linha de import existente é:

```rust
use crate::cli::args::RecordArgs;
```

Trocar por:

```rust
use crate::cli::args::{RecordArgs, SummarizeArgs};
```

- [ ] **Step 2: Adicionar `run_summarize`**

No fim de `runner.rs` (antes de `fn dir_size_bytes`):

```rust
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
```

- [ ] **Step 3: (USUÁRIO) compilar localmente**

Run (pelo usuário): `cargo check -p app_lib`
Expected: compila sem erros.

---

### Task 6: runner.rs — hook `record --summarize`

**Files:**
- Modify: `frontend/src-tauri/src/cli/runner.rs` (dentro de `run_record`, perto do fim)

**Interfaces:**
- Consumes: `summarize_existing_meeting` (Task 4); `args.summarize`, `args.template` (Task 1).

- [ ] **Step 1: Encadear o sumário no fim de `run_record`**

Em `run_record`, o trecho final é:

```rust
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
```

Inserir o bloco abaixo ENTRE o `}` que fecha o `else` e o `Ok(())`:

```rust
    // record --summarize: encadeia o sumário da reunião recém-gravada.
    // (validate() já garante !record_only quando summarize.) Falha aqui é apenas
    // warn — a gravação já foi persistida, então o comando sai com sucesso.
    if args.summarize {
        match summarize_existing_meeting(app, meeting_id, args.template.clone()).await {
            Ok(()) => println!("✓ Summary saved. meeting_id={}", meeting_id),
            Err(e) => eprintln!(
                "Warning: --summarize failed (the recording was saved): {}",
                e
            ),
        }
    }
```

Nota: `meeting_id` aqui é o `&str` já existente em `run_record`
(`let meeting_id = result.get("meeting_id").and_then(|v| v.as_str()).unwrap_or("?");`),
compatível com a assinatura `summarize_existing_meeting(app, meeting_id, ...)`.

- [ ] **Step 2: (USUÁRIO) compilar localmente**

Run (pelo usuário): `cargo check -p app_lib`
Expected: compila sem erros.

---

### Task 7: bin/meetily_cli.rs — dispatch dos comandos novos

**Files:**
- Modify: `frontend/src-tauri/src/bin/meetily_cli.rs`

**Interfaces:**
- Consumes: `run_summarize`, `run_list_meetings`, `run_list_templates`; `Command::{Summarize, ListMeetings, ListTemplates}`.

- [ ] **Step 1: Importar `SummarizeArgs` (se necessário) e adicionar os braços**

No `match cli.command.unwrap_or(...)`, depois do braço `Command::ListModels => { ... }` e antes de `Command::Record(args) => { ... }`, adicionar:

```rust
        Command::ListMeetings => {
            tauri::async_runtime::block_on(runner::run_list_meetings(&handle))
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::ListTemplates => {
            tauri::async_runtime::block_on(runner::run_list_templates())
                .unwrap_or_else(|e| eprintln!("error: {e}"));
        }
        Command::Summarize(args) => {
            if let Err(e) = tauri::async_runtime::block_on(runner::run_summarize(&handle, args)) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
```

A linha de import no topo já é:

```rust
use app_lib::cli::args::{Cli, Command, RecordArgs};
```

Não precisa mudar (os novos braços usam `Command::...`; `SummarizeArgs` é movido dentro de `Command::Summarize` sem nome explícito).

- [ ] **Step 2: (USUÁRIO) compilar localmente**

Run (pelo usuário): `cargo check -p app_lib --bin meetily-cli`
Expected: compila sem erros.

---

### Task 8: Verificação final e commit único

**Files:**
- (nenhuma alteração de código)

- [ ] **Step 1: (USUÁRIO) build + testes completos**

Run (pelo usuário): `cargo test -p app_lib cli::` e build do binário CLI conforme o fluxo local.
Expected: testes passam; binário compila.

- [ ] **Step 2: (USUÁRIO) smoke test manual**

```
meetily-cli list-templates
meetily-cli list-meetings
meetily-cli summarize --last
meetily-cli summarize --meeting <id> --template standard_meeting
meetily-cli record --duration 10 --summarize
```
Expected: `list-*` imprimem linhas; `summarize` mostra o spinner com tempo e termina com `✓ Summary saved...`; `record --summarize` grava e em seguida resume.

- [ ] **Step 3: Commit único de toda a feature**

```bash
git add frontend/src-tauri/src/cli/args.rs \
        frontend/src-tauri/src/cli/runner.rs \
        frontend/src-tauri/src/bin/meetily_cli.rs \
        docs/superpowers/specs/2026-06-19-cli-summarize-command-design.md \
        docs/superpowers/plans/2026-06-19-cli-summarize-command.md
git commit -m "feat(cli): comando summarize + list-meetings/list-templates + record --summarize"
```

---

## Self-Review

**Spec coverage:**
- `summarize --meeting/--last/--template` → Tasks 1, 4, 5. ✓
- `list-meetings`, `list-templates` → Task 2 + Task 7. ✓
- `record --summarize [--template]` → Tasks 1, 6. ✓
- Provider/model de `settings`, sem override → Task 4. ✓
- Só salva, sem imprimir markdown → reuso do pipeline (Task 3) + confirmação de 1 linha. ✓
- Spinner com elapsed → Task 3. ✓
- Tratamento de erros (banco vazio, id inexistente, transcript vazio, template inválido, modelo não configurado, falha LLM; record --summarize falha = warn) → Tasks 4, 5, 6. ✓
- Testes em `args.rs` → Task 1 Step 5. ✓

**Placeholder scan:** sem TODO/TBD; todo código presente. ✓

**Type consistency:** `summarize_meeting(app, meeting_id, text, provider, model, template_id)` usada igual em Tasks 3/4; `summarize_existing_meeting(app, meeting_id, template)` usada igual em Tasks 4/5/6; `save_transcript_data` com 7 args (`pool, meeting_id, text, model, model_name, chunk_size, overlap`); `get_summary_data` (sem JOIN) p/ confirmação; `process_transcript_background` retorna `()`. ✓
