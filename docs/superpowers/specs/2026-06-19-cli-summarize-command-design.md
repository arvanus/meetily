# CLI: comando de geração de sumário

**Data:** 2026-06-19
**Branch:** test/all-features
**Status:** Design aprovado (pendente revisão do spec escrito)

## Objetivo

Adicionar à `meetily-cli` a capacidade de gerar o sumário de uma reunião sem a UI,
reusando exatamente o pipeline de sumário do app (mesmo provider/model, mesmo
template, mesma persistência). O template é passável por parâmetro ou cai no
default. Inclui descoberta de ids (reuniões e templates) e encadeamento
automático após a gravação.

## Escopo

### Comandos

```
meetily-cli summarize [--meeting <id> | --last] [--template <id>]
meetily-cli list-meetings
meetily-cli list-templates
meetily-cli record ... [--summarize] [--template <id>]
```

- **`summarize`**: gera o sumário de uma reunião existente.
  - `--meeting <id>`: id explícito.
  - `--last`: reunião mais recente (`get_meetings()[0]`, já ordenado por
    `created_at DESC`).
  - `--template <id>`: default `daily_standup` (mesmo default do app, vide
    `api_process_transcript`). Id inválido → erro listando os ids válidos.
- **`list-meetings`**: lista `id | created_at | title` para descobrir ids.
- **`list-templates`**: lista `id | name | description` (built-in + custom).
- **`record --summarize`**: após o stop+save normal, encadeia o mesmo fluxo de
  `summarize` sobre a reunião recém-gravada (o `meeting_id` já é conhecido).
  Reusa a flag `--template` do `record`.

### Fora de escopo (YAGNI)

- Override de provider/model do LLM via flags. O motor vem **sempre** da config
  do app (tabela `settings`, via `SettingsRepository::get_model_config`).
- Impressão do markdown no stdout. O comando **só salva** (banco + `summary.md`),
  igual ao estilo do `record`.
- Edição/criação de templates pela CLI.

## Abordagem escolhida

**Reusar o caminho do app, mas aguardando (em vez de `spawn`).**

`api_process_transcript` (commands.rs) faz hoje:
1. `create_or_reset_process`
2. `save_transcript_data`
3. `spawn(process_transcript_background(...))` e retorna `process_id`

A CLI espelha (1) e (2), depois **`await`** direto em
`SummaryService::process_transcript_background(...)` em vez de `spawn`, e ao
final lê o status persistido para confirmar. Isso reusa 100% da lógica testada:
resolução de api key, carregamento de contexto (textarea + anexos), token
threshold por provider, persistência no banco e export do `summary.md`.

Rejeitada a alternativa de chamar `generate_meeting_summary` direto e persistir à
mão (duplica a persistência `update_process_completed` + `file_export` e diverge
do app).

## Detalhamento

### args.rs

Novos membros do enum `Command`:

```rust
/// Generate the summary of a meeting (reuses the app's summary pipeline)
Summarize(SummarizeArgs),
/// List meetings (id, date, title) to discover meeting ids
ListMeetings,
/// List available summary templates (id, name, description)
ListTemplates,
```

`SummarizeArgs`:

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
            (true, true) => Err("use either --meeting <id> or --last, not both.".into()),
            (false, false) => Err("specify the meeting to summarize: --meeting <id> or --last.".into()),
            _ => Ok(()),
        }
    }
}
```

`RecordArgs`: adicionar
```rust
/// After saving, generate the summary of the just-recorded meeting
#[arg(long)]
pub summarize: bool,
/// Template id used when --summarize is set (default: "daily_standup")
#[arg(long)]
pub template: Option<String>,
```

`RecordArgs::validate()`: adicionar
```rust
if self.summarize && self.record_only {
    return Err("--summarize needs a transcription; it is incompatible with --record-only.".into());
}
```

### runner.rs

**`run_summarize(app, args: SummarizeArgs)`**
1. `args.validate()?`.
2. Resolver `meeting_id`:
   - `--meeting <id>`: usa direto.
   - `--last`: `MeetingsRepository::get_meetings(pool)`; vazio → erro
     ("no meetings found."). Pega `[0].id`.
3. `MeetingsRepository::get_meeting(pool, &meeting_id)`:
   - `None` → erro ("meeting <id> not found.").
   - concatena `transcripts[].text` (separados por `\n`) em `text`. Vazio →
     erro ("this meeting has no transcript to summarize; re-transcribe it in the app.").
4. Resolver template: `args.template.unwrap_or("daily_standup")`. Validar com
   `templates::get_template(&id)`; erro → mensagem com os ids de
   `templates::list_templates()`.
5. Resolver provider/model: `SettingsRepository::get_model_config(pool)`:
   - `None` ou provider/model vazios → erro ("configure the summary model in the
     app first.").
6. Chamar o helper compartilhado `summarize_meeting(...)` (abaixo).
7. Imprimir confirmação de uma linha.

**`summarize_meeting(app, meeting_id, text, provider, model, template_id) -> Result<(), String>`**
(helper reusado por `run_summarize` e `record --summarize`)
1. `SummaryProcessesRepository::create_or_reset_process(pool, &meeting_id)`.
2. `TranscriptChunksRepository::save_transcript_data(pool, &meeting_id, &text,
   &provider, &model, 40000, 1000)` (mesmos defaults de `api_process_transcript`).
3. Inicia a **draw task do spinner** (ver abaixo).
4. `await SummaryService::process_transcript_background(app, pool, meeting_id,
   text, provider, model, template_id)`.
5. Aborta a draw task, limpa a linha.
6. `SummaryProcessesRepository::get_summary_data_for_meeting(pool, &meeting_id)`:
   - status `completed` → `Ok(())`.
   - status `failed` → `Err(error_msg)`.
   - outro → `Err("summary did not complete.")`.

**Spinner (draw task, padrão do `run_record`)**
- `tokio::spawn` com `tokio::time::interval(250ms)`.
- A cada tick: `print!("\r\x1b[K⏳ Generating summary… {elapsed}s {frame}")` +
  `flush`, onde `frame` cicla em `['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏']`
  (ou os pulsos já usados no painel) e `elapsed` em segundos desde o início.
- `draw_handle.abort()` + `print!("\r\x1b[K")` ao terminar, igual ao `run_record`.

**`run_list_meetings(app)`**
- `MeetingsRepository::get_meetings(pool)`.
- Cabeçalho + uma linha por reunião: `id   created_at   title`.
- Vazio → `(none)`.

**`run_list_templates(app)`**
- `templates::list_templates()` → `(id, name, description)`.
- Uma linha por template: `- <id>  —  <name>: <description>`.

### record --summarize (runner::run_record)

Ao final do caminho de sucesso (após o `✓ Saved. meeting_id=… folder=…`):
- Se `args.summarize` (já garantido `!record_only` por `validate`):
  - resolve provider/model de `get_model_config`; ausente → warn e pula
    (a gravação já foi salva; não falha o comando).
  - concatena o transcript da reunião recém-criada (reusa o `segments_vec` já
    acumulado **ou** relê via `get_meeting(meeting_id)` — usar o relê para
    garantir paridade com o caminho do `summarize`).
  - chama `summarize_meeting(...)` com `args.template`.
  - confirmação própria; falha do sumário vira warn (não derruba o exit 0 da
    gravação já salva).

### bin/meetily_cli.rs

Adicionar os braços no `match`:
```rust
Command::Summarize(args) => run_summarize(&handle, args) ...
Command::ListMeetings    => run_list_meetings(&handle) ...
Command::ListTemplates   => run_list_templates(&handle) ...
```

## Tratamento de erros

- Banco vazio (`--last`): mensagem clara, exit 1.
- `meeting_id` inexistente: mensagem clara, exit 1.
- Transcript vazio: mensagem clara, exit 1.
- Template inválido: erro listando ids válidos, exit 1.
- Config de modelo ausente: erro pedindo configuração no app, exit 1.
- Falha do LLM (`process_transcript_background` → status `failed`): propaga a
  mensagem do banco, exit 1.
- `record --summarize`: falha do sumário é **warn**, não derruba a gravação
  (que já foi persistida); exit 0.

## Testes (mod tests em args.rs)

- `summarize` sem `--meeting`/`--last` → `validate()` é `Err`.
- `summarize` com ambos → `validate()` é `Err`.
- `summarize --last` → `Ok`, `last == true`.
- `summarize --meeting x` → `Ok`, `meeting == Some("x")`.
- `--template` ausente resolve para `daily_standup` (testar o default na função
  de resolução, ou via parse + unwrap_or).
- `record --summarize --record-only` → `validate()` é `Err`.
- `record --summarize` (sem `--record-only`) → `Ok`.

## Arquivos tocados

- `frontend/src-tauri/src/cli/args.rs` — comandos, args, validate, testes.
- `frontend/src-tauri/src/cli/runner.rs` — `run_summarize`, `summarize_meeting`,
  `run_list_meetings`, `run_list_templates`, spinner, hook no `run_record`.
- `frontend/src-tauri/src/bin/meetily_cli.rs` — dispatch dos novos comandos.

Nenhuma mudança no pipeline de sumário do app (reuso puro).
