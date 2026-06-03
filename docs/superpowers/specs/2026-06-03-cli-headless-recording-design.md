# Design: CLI headless de gravação + transcrição (`meetily-cli`)

**Data:** 2026-06-03
**Status:** Aprovado para planejamento
**Branch alvo:** a definir (provável `enhance/cli-headless`)

## Problema / motivação

Hoje, gravar e transcrever exige rodar o app Tauri inteiro (Next.js + WebView2).
O usuário quer um CLI que:

1. Inicie a gravação automaticamente, sem abrir a UI.
2. Mostre a transcrição em tempo real no terminal.
3. Salve **igual a uma gravação do app** — mesma pasta de gravação (`transcripts.json`,
   `audio.mp4`, `metadata.json`) **e** o mesmo banco SQLite, de modo que a reunião
   apareça na lista do app.

Objetivo secundário forte: um modo **"gravar agora, transcrever depois"** que consome
RAM mínima por **não carregar nenhum modelo de IA** durante a captura.

## Descobertas-chave do código (base do design)

- A lógica de captura/mixagem/VAD/Whisper/Parakeet e a escrita de `transcripts.json`
  são **agnósticas de UI**. O acoplamento ao app é via `AppHandle` (emissão de eventos)
  e `tauri::State`.
- **Persistência no DB é feita no Rust, não pelo backend Python.**
  `api_save_transcript` (`src/api/api.rs:934`) grava direto via
  `state.db_manager.pool()` → `TranscriptsRepository::save_transcript`.
  `api_get_meetings` (`src/api/api.rs:326`) **lê do mesmo pool**. Logo, a lista de
  reuniões do app vem do SQLite gerido pelo Rust — **o backend FastAPI (5167) não
  precisa estar rodando** para a reunião aparecer no app.
- O DB e a pasta de modelos são resolvidos por `app_data_dir()`
  (`DatabaseManager::new_from_app_handle`, `src/database/manager.rs:44` →
  `meeting_minutes.sqlite`). Um binário Tauri com o **mesmo `identifier`
  (`com.meetily.ai`)** cai automaticamente na mesma pasta de dados → mesmo banco,
  mesmos modelos. Reuso praticamente total.
- O motor de transcrição é escolhido pela config (`api_get_transcript_config`,
  `provider: "whisper" | "parakeet"`). O CLI reusa o mesmo worker → **Whisper e
  Parakeet funcionam idênticos**, sem código específico.
- Re-transcrição já existe: `start_retranscription(app, meeting_id,
  meeting_folder_path, language, model, provider)` (`src/audio/retranscription.rs:90`)
  parte do **arquivo de áudio salvo na pasta** (`find_audio_file` → decode → VAD →
  transcreve). É isso que viabiliza "gravar sem IA agora, transcrever depois".
- Feedback de áudio já é emitido: evento `audio-levels` com `mic_rms` e `system_rms`
  (0.0–1.0) em `src/audio/recording_commands.rs:321-329`. Roda **independente da
  transcrição** (vale até no `--record-only`).
- Dependências já presentes no `Cargo.toml`: `clap 4.3` (derive), `realfft 3.4.0`,
  `tokio` (full), `sqlx` (sqlite). Padrão de uso de `RealFftPlanner` em
  `src/audio/audio_processing.rs:412-428` (usado hoje para redução de ruído).
- **Não** há ainda um `[[bin]]` separado; só `[lib]` (cdylib/rlib para o Tauri).

## Arquitetura

Novo binário **`[[bin]] meetily-cli`** dentro de `frontend/src-tauri/`, que sobe o
runtime Tauri **sem criar nenhuma janela/webview**, registra os mesmos `State` do app
e roda a gravação no `setup()`. Mesmo `identifier` → mesmo `app_data_dir`/DB/modelos.

```
meetily-cli  (mesmo identifier do app → mesmo DB e pasta de modelos)
   │
   ├─ tauri::Builder (headless: nenhum WebviewWindow criado)
   │     .manage(AppState{ db_manager })          ← mesmo SQLite do app
   │     .manage(ParallelProcessorState, ModelManagerState,
   │              system_audio_state, NotificationManagerState…)
   │
   └─ setup(app):
        1. (modo normal) start_recording_with_meeting_name(app, nome)  ← reuso total
           (modo --record-only) inicia só captura/saver, SEM iniciar transcrição
        2. app.listen("transcript-update")  → imprime fala no terminal (tempo real)
        3. app.listen("audio-levels")        → alimenta o painel vivo (VU + FFT)
        4. Ctrl+C (ou --duration) → stop_recording(app)
              → recording_saver grava transcripts.json/metadata.json/audio.mp4
              → TranscriptsRepository::save_transcript (mesmo pool) → aparece no app
```

`app.emit(...)` sem ouvintes vira no-op (não quebra). O CLI atua como o "frontend",
porém consumindo os eventos via listeners Rust e imprimindo no terminal.

## Interface de linha de comando (clap, subcomandos)

```
meetily-cli record [flags]        # subcomando default; "record" pode ser omitido
  --name <texto>                  # default: "CLI Meeting <data/hora>"
  --record-only                   # grava SEM carregar IA (RAM mínima); re-transcreve depois
  --engine <whisper|parakeet>     # default: o configurado no app
  --model <nome>                  # default: o configurado no app
  --mic <device>                  # default: dispositivo padrão do SO
  --system <device>               # default: loopback padrão
  --partial                       # mostra transcrições parciais ao vivo
  --quiet                         # não imprime transcrição (painel vivo CONTINUA)
  --duration <segundos>           # para automaticamente após N s (default: até Ctrl+C)
  --no-audio-save                 # só transcreve, não grava .mp4 (incompatível com --record-only)

meetily-cli list-models           # lista whisper + parakeet disponíveis, marca o default
meetily-cli list-devices          # lista dispositivos de áudio
```

- Sem flags: mic+sistema padrão, nome automático, grava áudio + transcreve + salva no
  DB, imprime falas finais, encerra com Ctrl+C.
- `list-models` mapeia para `whisper_get_available_models` +
  `parakeet_get_available_models`; o default vem de `api_get_transcript_config`.

## Modos de operação

### Normal (com IA)
Carrega o motor configurado (ou o de `--engine/--model`), roda a pipeline completa
(mixagem + VAD + transcrição) e persiste tudo. Falas finais rolam no terminal acima
do painel vivo.

### `--record-only` (gravar sem IA — o "PLUS")
- **Nenhum modelo de IA é carregado** → RAM mínima (só captura + saver; sem
  Whisper/Parakeet na memória, sem VAD-para-transcrição).
- Salva `audio.mp4` (obrigatório — é a fonte da re-transcrição) + `metadata.json` +
  `transcripts.json` **vazio**, e **cria a reunião no DB**.
- Resultado: a reunião aparece no app com transcrição zerada; o usuário re-transcreve
  depois pelo app (botão já existente → `start_retranscription`), escolhendo o modelo.
- **Incompatível com `--no-audio-save`** (a CLI deve rejeitar a combinação com erro
  claro).

> Re-transcrição via CLI fica **fora da v1** (decisão do usuário): o passo 2 é feito no
> app. A função Rust já existe, então um subcomando `retranscribe` é melhoria futura barata.

## Feedback visual (painel vivo)

Linha de status fixada no rodapé, atualizada ~4×/s a partir do evento `audio-levels`,
com três sinais independentes de vida:

```
● REC  00:03:12  │ graves ▆  médios ▃  agudos ▂ │ mic ▮▮▮▯  sis ▮▮▯▯ │ trechos:12 │ whisper/base.en
```

- **`●` que pisca a cada segundo** → prova que o loop está vivo mesmo em silêncio total.
- **Equalizador FFT de 3 bandas** (graves/médios/agudos) do mix → "vivo" e bonito.
- **Medidores VU de mic e sistema** (RMS) → mostra que o áudio entra e separa as fontes.
- **Contador de trechos** sobe a cada `transcript-update`. No `--record-only` esse
  campo vira **`gravado: 4.2 MB / 00:03:12`** (sem transcrição).
- **Alerta `⚠ sem áudio há Ns`** se os RMS zerarem por muito tempo.

Com `--quiet`, some o texto das falas mas o painel continua (é o ponto do feedback).

**Banner de início** confirmando o que subiu (motor/modelo, dispositivos, reunião, pasta):

```
✓ Motor: whisper (base.en)   ✓ Mic: Realtek   ✓ Sistema: loopback
✓ Reunião: "CLI Meeting 2026-06-03 14:30"  → …\Meetily\…\Meeting_…
Gravando. Ctrl+C para parar e salvar.
```

### FFT
- Reusar `realfft` (já é dependência); padrão de `RealFftPlanner` em
  `audio_processing.rs:412-428`.
- O usuário indicou existir código de FFT de "5 barras" — **localizar na
  implementação** e reusar a divisão de bandas. Renderizar 3 bandas; se o código de 5
  bandas for trivial de reaproveitar, render 5 — **decisão cosmética de implementação**.
- Fonte das amostras para FFT: o CLI é o próprio processo que grava, então as amostras
  do mix estão no processo (tap no caminho que alimenta o `recording_saver`/pipeline).
  **Detalhe a resolver no plano:** de onde exatamente tapar as amostras sem perturbar a
  pipeline.

## Tratamento de erros e parada

- **Parada limpa:** handler `tokio::signal` (Ctrl+C) → `stop_recording` → grava arquivos
  + salva no DB. Imprime `meeting_id` e caminho da pasta no fim. `--duration` dispara o
  mesmo caminho por timeout.
- **Backend Python fora (5167):** irrelevante para gravar/transcrever/salvar no DB.
  Afetaria só sumarização (fora de escopo do CLI).
- **Falha de dispositivo de áudio:** mensagem clara, exit code ≠ 0.
- **Modelo não baixado:** erro orientando baixar pelo app. **O CLI não baixa modelos na
  v1** (YAGNI).

## Escopo explicitamente fora da v1

- Não baixa modelos.
- Não gera sumário (LLM).
- Re-transcrição **via CLI** (feita no app na v1).

## Questões abertas (resolver no plano)

1. Construir um `tauri::App` headless (sem `WebviewWindow`) e obter `AppHandle` válido —
   confirmar API exata (`Builder::build` + `run`/iconless) e quais `State`s são
   estritamente obrigatórios para `start_recording`/`stop_recording` não entrarem em
   pânico.
2. `--record-only`: garantir que `TranscriptsRepository::save_transcript` (ou caminho
   equivalente) crie a linha da reunião com transcrição vazia, de forma que o app
   ofereça re-transcrição apontando para a pasta correta.
3. Onde tapar as amostras do mix para o FFT sem perturbar a pipeline; localizar o código
   de FFT de 5 barras citado pelo usuário.
4. Renderização do painel fixo no terminal (Windows PowerShell): ANSI/`\r` manual vs.
   adicionar `crossterm` (não está nas deps hoje).
5. Confirmar inicialização dos diretórios de modelos no contexto headless
   (`whisper_engine::commands::set_models_directory` / equivalente Parakeet usam
   `AppHandle`).
```
