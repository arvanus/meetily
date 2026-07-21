# Contexto de resumo em reuniões sem transcrição — Plano de Implementação

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Permitir que uma reunião sem transcrição (ex.: gravada pelo CLI) exiba o campo de contexto/anexos e gere resumo a partir desse conteúdo colado.

**Architecture:** Duas funções puras novas no Rust (`render_transcript_block`, `has_summarizable_content`) em `summary/context/prompt_builder.rs`, cobertas por `cargo test`, trocam o bloco `<transcript_chunks>` por um marcador `<no_transcript>` quando não há transcrição e barram geração vazia no backend. No frontend, um predicado puro `canGenerateSummary` em `lib/summary-generation.ts` substitui a checagem `transcripts.length > 0` espalhada por três componentes, e o painel de contexto deixa de depender da contagem de segmentos.

**Tech Stack:** Rust (Tauri 2.x, `cargo test`), TypeScript/React 18 (Next.js 14), sem runner de testes no frontend.

**Spec:** [docs/superpowers/specs/2026-07-21-summary-context-without-transcript-design.md](../specs/2026-07-21-summary-context-without-transcript-design.md)

## Global Constraints

- **Sem transcrição, o texto colado permanece contexto.** Nada é gravado na tabela `transcripts`; o painel de transcrição continua vazio e "Copy Transcript" continua desabilitado.
- **Prompt com transcrição é byte-idêntico ao atual.** Qualquer mudança visível no prompt quando `text` não está vazio é regressão.
- **Marcador exato** a emitir sem transcrição (texto literal, em inglês, como o resto dos prompts do arquivo):
  ```
  <no_transcript>
  This meeting has no transcript. Base the summary entirely on the
  user-provided context and attachments below.
  </no_transcript>
  ```
- **Mensagem de erro única** para "nada a resumir": no frontend `Nada para resumir: adicione contexto ou um anexo`; no Rust `Nothing to summarize: transcript, context and attachments are all empty`.
- **Sem infra de teste nova no frontend.** O projeto não tem vitest/jest e não vamos adicionar. Validação de frontend = `npx tsc --noEmit` + checklist manual da Task 6.
- **Baseline do `tsc`:** rodar `pnpm install` antes de qualquer coisa. O `git pull` recente adicionou `react-day-picker@^10.0.1` ao `package.json` sem instalar; sem isso o `tsc` reporta 4 erros pré-existentes em `src/components/Sidebar/index.tsx` e `src/components/ui/calendar.tsx` que **não** são desta feature.
- **`cargo test` sem features de GPU.** Rodar sempre `cargo test` puro (nunca `--features cuda`): a máquina tem 16 GB e builds CUDA paralelos estouram memória. Não existe `target/` hoje, então a primeira compilação é fria — sccache está configurado em `.cargo/config.toml` e ajuda nas seguintes.

## File Structure

| Arquivo | Responsabilidade |
|---|---|
| `frontend/src-tauri/src/summary/context/prompt_builder.rs` | **Modificar.** Já renderiza o bloco `<attachments>`; ganha o bloco de transcrição e o predicado de conteúdo. É o único módulo de montagem de prompt que é puro e testável — a casa natural dos dois. |
| `frontend/src-tauri/src/summary/processor.rs` | **Modificar** em 2 pontos: guard no início de `generate_meeting_summary` (~linha 184) e substituição do `format!` de `<transcript_chunks>` (linhas 359-364). |
| `frontend/src/lib/summary-generation.ts` | **Criar.** Predicado puro `canGenerateSummary`, consumido por `page-content.tsx`. Módulo próprio (e não inline no componente) porque a regra é a mesma que o hook precisa respeitar. |
| `frontend/src/components/MeetingDetails/TranscriptPanel.tsx` | **Modificar** linha 162: condição de exibição do painel de contexto. |
| `frontend/src/components/MeetingDetails/SummaryGeneratorButtonGroup.tsx` | **Modificar** linhas 37, 52, 76: prop `hasTranscripts` → `canGenerateSummary`. |
| `frontend/src/components/MeetingDetails/SummaryPanel.tsx` | **Modificar** interface + linhas 115, 154, 181: para de derivar de `transcripts.length`, recebe a prop pronta. |
| `frontend/src/app/meeting-details/page-content.tsx` | **Modificar.** Único ponto com transcrições + contexto + anexos à mão; computa o predicado e distribui. |
| `frontend/src/hooks/meeting-details/useSummaryGeneration.ts` | **Modificar** linhas 13-35 (props), 72, 411-418, 579: aceita `canGenerate` e para de abortar por falta de transcrição. |

**Consumidores únicos (verificado):** `MeetingDetails/TranscriptPanel` e `SummaryPanel` só são usados em `page-content.tsx` (linhas 177 e 203). O `TranscriptPanel` importado por `src/app/page.tsx:14` é outro componente (`app/_components/TranscriptPanel`) e não é afetado. Por isso as props novas podem ser obrigatórias.

---

### Task 1: Bloco `<no_transcript>` e guard de conteúdo (Rust)

**Files:**
- Modify: `frontend/src-tauri/src/summary/context/prompt_builder.rs`
- Modify: `frontend/src-tauri/src/summary/processor.rs:184`, `frontend/src-tauri/src/summary/processor.rs:359-364`

**Interfaces:**
- Consumes: `AttachmentContent { display_name: String, content: String, truncated: bool }`, já importado no topo de `prompt_builder.rs`.
- Produces:
  ```rust
  pub fn render_transcript_block(content: &str) -> String
  pub fn has_summarizable_content(
      text: &str,
      context_prompt: &str,
      attachments: &[AttachmentContent],
  ) -> bool
  ```

- [ ] **Step 1: Escrever os testes que falham**

Adicionar ao final do `mod tests` existente em `prompt_builder.rs` (antes do `}` que fecha o módulo, depois de `renders_multiple_files_in_order`):

```rust
    #[test]
    fn transcript_block_wraps_content_when_present() {
        let block = render_transcript_block("hello world");
        assert!(block.starts_with("<transcript_chunks>\n"));
        assert!(block.contains("hello world"));
        assert!(block.ends_with("</transcript_chunks>"));
        assert!(!block.contains("<no_transcript>"));
    }

    #[test]
    fn transcript_block_emits_marker_when_empty() {
        let block = render_transcript_block("   \n\t ");
        assert!(block.contains("<no_transcript>"));
        assert!(block.contains("</no_transcript>"));
        assert!(!block.contains("<transcript_chunks>"));
    }

    #[test]
    fn has_content_is_false_when_all_sources_are_empty() {
        assert!(!has_summarizable_content("   ", "  \n ", &[]));
    }

    #[test]
    fn has_content_is_true_with_only_transcript() {
        assert!(has_summarizable_content("some transcript", "", &[]));
    }

    #[test]
    fn has_content_is_true_with_only_context() {
        assert!(has_summarizable_content("", "pasted transcript", &[]));
    }

    #[test]
    fn has_content_is_true_with_only_attachment() {
        let attachments = [AttachmentContent {
            display_name: "brief.md".to_string(),
            content: "x".to_string(),
            truncated: false,
        }];
        assert!(has_summarizable_content("", "", &attachments));
    }
```

- [ ] **Step 2: Rodar os testes e confirmar que falham**

```bash
cargo test --manifest-path frontend/src-tauri/Cargo.toml --lib prompt_builder
```

Esperado: falha de compilação — `cannot find function 'render_transcript_block' in this scope` e `cannot find function 'has_summarizable_content' in this scope`.

- [ ] **Step 3: Implementar as duas funções**

Inserir em `prompt_builder.rs` logo após `render_attachments_block` (antes do `#[cfg(test)]`):

```rust
/// Wraps the transcript in `<transcript_chunks>`. When there is no transcript,
/// emits a `<no_transcript>` marker instead, telling the model to rely on the
/// user-provided context and attachments that follow.
pub fn render_transcript_block(content: &str) -> String {
    if content.trim().is_empty() {
        return "<no_transcript>\nThis meeting has no transcript. Base the summary entirely on the\nuser-provided context and attachments below.\n</no_transcript>"
            .to_string();
    }
    format!("<transcript_chunks>\n{}\n</transcript_chunks>", content)
}

/// True when at least one source of content exists to summarize.
pub fn has_summarizable_content(
    text: &str,
    context_prompt: &str,
    attachments: &[AttachmentContent],
) -> bool {
    !text.trim().is_empty() || !context_prompt.trim().is_empty() || !attachments.is_empty()
}
```

- [ ] **Step 4: Rodar os testes e confirmar que passam**

```bash
cargo test --manifest-path frontend/src-tauri/Cargo.toml --lib prompt_builder
```

Esperado: `test result: ok.` com 11 testes (os 5 que já existiam + os 6 novos).

- [ ] **Step 5: Ligar no `processor.rs` — bloco de transcrição**

Substituir as linhas 359-364, que hoje são:

```rust
    final_user_prompt.push_str(&format!(
        r#"<transcript_chunks>
{}
</transcript_chunks>"#,
        content_to_summarize
    ));
```

por:

```rust
    final_user_prompt.push_str(&crate::summary::context::prompt_builder::render_transcript_block(
        &content_to_summarize,
    ));
```

- [ ] **Step 6: Ligar no `processor.rs` — guard**

Em `generate_meeting_summary`, logo depois do `info!("Starting summary generation with provider: ...")` (linha ~188) e antes de `let total_tokens = rough_token_count(text);`:

```rust
    if !crate::summary::context::prompt_builder::has_summarizable_content(
        text,
        context_prompt,
        attachments,
    ) {
        return Err(
            "Nothing to summarize: transcript, context and attachments are all empty".to_string(),
        );
    }
```

- [ ] **Step 7: Compilar e rodar a suíte do módulo**

```bash
cargo test --manifest-path frontend/src-tauri/Cargo.toml --lib summary::
```

Esperado: compila sem erro e `test result: ok.`. Se aparecer `unused import` ou warning novo, corrigir antes de commitar.

- [ ] **Step 8: Commit**

```bash
git add frontend/src-tauri/src/summary/context/prompt_builder.rs frontend/src-tauri/src/summary/processor.rs
git commit -m "feat(summary): marcador <no_transcript> e guard de conteudo vazio"
```

---

### Task 2: Predicado `canGenerateSummary` (frontend)

**Files:**
- Create: `frontend/src/lib/summary-generation.ts`

**Interfaces:**
- Consumes: nada.
- Produces:
  ```ts
  export function canGenerateSummary(params: {
    transcriptCount: number;
    contextPrompt: string;
    attachmentCount: number;
  }): boolean
  ```
  As Tasks 4 e 5 dependem deste nome e desta assinatura exata.

- [ ] **Step 1: Criar o módulo**

`frontend/src/lib/summary-generation.ts`:

```ts
/**
 * A meeting can be summarized when it has at least one content source:
 * transcript segments, free-form context text, or context attachments.
 *
 * Mirrors `has_summarizable_content` in
 * `src-tauri/src/summary/context/prompt_builder.rs` — the backend enforces the
 * same rule, this predicate only keeps the UI from offering a doomed action.
 */
export function canGenerateSummary(params: {
  transcriptCount: number;
  contextPrompt: string;
  attachmentCount: number;
}): boolean {
  return (
    params.transcriptCount > 0 ||
    params.contextPrompt.trim() !== '' ||
    params.attachmentCount > 0
  );
}
```

- [ ] **Step 2: Verificar tipos**

```bash
cd frontend && npx tsc --noEmit -p tsconfig.json
```

Esperado: nenhum erro novo. Se `pnpm install` já rodou, saída vazia; caso contrário, apenas os 4 erros de baseline em `Sidebar/index.tsx` e `ui/calendar.tsx` (ver Global Constraints).

- [ ] **Step 3: Commit**

```bash
git add frontend/src/lib/summary-generation.ts
git commit -m "feat(summary): predicado canGenerateSummary"
```

---

### Task 3: Exibir o painel de contexto sem transcrição

**Files:**
- Modify: `frontend/src/components/MeetingDetails/TranscriptPanel.tsx:162`

**Interfaces:**
- Consumes: nada das tasks anteriores.
- Produces: nada consumido por tasks posteriores.

- [ ] **Step 1: Trocar a condição**

Linha 162, hoje:

```tsx
      {!isRecording && convertedSegments.length > 0 && (
```

passa a:

```tsx
      {!isRecording && (
```

Nada mais muda no arquivo: o `convertedSegments` continua sendo usado pelo `VirtualizedTranscriptView` (linha 145) e pelo `TranscriptButtonGroup` (linha 133), então não vira variável morta.

- [ ] **Step 2: Verificar tipos**

```bash
cd frontend && npx tsc --noEmit -p tsconfig.json
```

Esperado: nenhum erro novo em relação ao baseline.

- [ ] **Step 3: Commit**

```bash
git add frontend/src/components/MeetingDetails/TranscriptPanel.tsx
git commit -m "feat(ui): exibir campo de contexto em reunioes sem transcricao"
```

---

### Task 4: Gate de geração por conteúdo, não por transcrição

**Files:**
- Modify: `frontend/src/components/MeetingDetails/SummaryGeneratorButtonGroup.tsx:37,52,76`
- Modify: `frontend/src/components/MeetingDetails/SummaryPanel.tsx` (interface + linhas 115, 154, 181)
- Modify: `frontend/src/app/meeting-details/page-content.tsx`

**Interfaces:**
- Consumes: `canGenerateSummary` de `@/lib/summary-generation` (Task 2).
- Produces: a constante `canGenerate: boolean` dentro de `page-content.tsx`, reaproveitada pela Task 5.

- [ ] **Step 1: Renomear a prop em `SummaryGeneratorButtonGroup.tsx`**

Linha 37, na interface:

```ts
  canGenerateSummary?: boolean;
```

Linha 52, na desestruturação:

```ts
  canGenerateSummary = true,
```

Linha 76:

```tsx
  if (!canGenerateSummary) {
    return null;
  }
```

- [ ] **Step 2: Repassar a prop em `SummaryPanel.tsx`**

Na interface `SummaryPanelProps`, logo depois de `transcripts: Transcript[];` (linha 32):

```ts
  canGenerateSummary: boolean;
```

Na desestruturação, logo depois de `transcripts,` (linha 67):

```ts
  canGenerateSummary,
```

E nas três instâncias do componente filho (linhas 115, 154 e 181), trocar:

```tsx
                hasTranscripts={transcripts.length > 0}
```

por:

```tsx
                canGenerateSummary={canGenerateSummary}
```

A prop `transcripts` continua no componente — é usada em outros pontos do arquivo; não removê-la.

- [ ] **Step 3: Computar o predicado em `page-content.tsx`**

Adicionar o import junto aos demais imports do topo do arquivo:

```ts
import { canGenerateSummary } from '@/lib/summary-generation';
```

Logo após a chamada de `useMeetingData`/`useTemplates` (linhas 74-75) e **antes** de `useSummaryGeneration` (linha 116):

```ts
  // Resumo pode ser gerado com transcrição, contexto colado ou anexos.
  const canGenerate = canGenerateSummary({
    transcriptCount: meetingData.transcripts.length,
    contextPrompt: summaryContext.contextPrompt,
    attachmentCount: summaryContext.attachments.length,
  });
```

E passar para o `SummaryPanel` (a partir da linha 203), junto das props que já existem:

```tsx
          canGenerateSummary={canGenerate}
```

- [ ] **Step 4: Verificar tipos**

```bash
cd frontend && npx tsc --noEmit -p tsconfig.json
```

Esperado: nenhum erro novo. Um erro do tipo `Property 'canGenerateSummary' is missing` significa que o Step 3 não passou a prop; um `'hasTranscripts' does not exist` significa que sobrou um uso antigo — procurar com `grep -rn hasTranscripts frontend/src`, que deve voltar vazio.

- [ ] **Step 5: Commit**

```bash
git add frontend/src/components/MeetingDetails/SummaryGeneratorButtonGroup.tsx frontend/src/components/MeetingDetails/SummaryPanel.tsx frontend/src/app/meeting-details/page-content.tsx
git commit -m "feat(summary): habilitar geracao por contexto/anexos, nao so transcricao"
```

---

### Task 5: Hook de geração aceita reunião sem transcrição

**Files:**
- Modify: `frontend/src/hooks/meeting-details/useSummaryGeneration.ts:13-35,72,411-418,579`
- Modify: `frontend/src/app/meeting-details/page-content.tsx:116-126`

**Interfaces:**
- Consumes: a constante `canGenerate` criada na Task 4.
- Produces: nada consumido depois.

- [ ] **Step 1: Aceitar a nova prop no hook**

Em `UseSummaryGenerationProps` (linha 13), depois de `transcripts: Transcript[];`:

```ts
  canGenerate: boolean;
```

Na desestruturação (linha 25), depois de `transcripts,`:

```ts
  canGenerate,
```

- [ ] **Step 2: Afrouxar o guard de `processSummary`**

Linhas 72-74, hoje:

```ts
      if (!transcriptText.trim()) {
        throw new Error('No transcript text available. Please add some text first.');
      }
```

passam a:

```ts
      // Sem transcrição ainda é válido: o backend monta o prompt a partir do
      // contexto persistido e dos anexos.
      if (!transcriptText.trim() && !canGenerate) {
        throw new Error('Nada para resumir: adicione contexto ou um anexo');
      }
```

- [ ] **Step 3: Afrouxar o guard de `handleGenerateSummary`**

Linhas 411-418, hoje:

```ts
    if (!allTranscripts.length) {
      const error_msg = 'No transcripts available for summary';
      console.log(error_msg);
      toast.error(error_msg);
      return;
    }

    console.log(`✅ Proceeding with ${allTranscripts.length} transcripts`);
```

passam a:

```ts
    if (!allTranscripts.length && !canGenerate) {
      const error_msg = 'Nada para resumir: adicione contexto ou um anexo';
      console.log(error_msg);
      toast.error(error_msg);
      return;
    }

    console.log(
      allTranscripts.length > 0
        ? `✅ Proceeding with ${allTranscripts.length} transcripts`
        : '✅ Proceeding without transcript (context/attachments only)'
    );
```

Com zero transcrições, o `fullTranscript` montado na linha 566 já resulta em string vazia — nenhuma mudança necessária ali.

- [ ] **Step 4: Liberar a regeneração sem transcrição**

Linhas 579-582, hoje:

```ts
    if (!originalTranscript.trim()) {
      console.error('No original transcript available for regeneration');
      return;
    }
```

passam a:

```ts
    if (!originalTranscript.trim() && !canGenerate) {
      console.error('Nothing to regenerate: no transcript, context or attachments');
      return;
    }
```

Sem isso, um resumo gerado só a partir do contexto nunca poderia ser regerado — `originalTranscript` fica `''` nesse caminho.

- [ ] **Step 5: Atualizar as dependências dos `useCallback`**

Adicionar `canGenerate` ao array de dependências de `processSummary`, `handleGenerateSummary` (linha 575) e `handleRegenerateSummary` (linha 588). Exemplo para o último:

```ts
  }, [originalTranscript, canGenerate, processSummary]);
```

- [ ] **Step 6: Passar a prop na chamada do hook**

Em `page-content.tsx`, na chamada de `useSummaryGeneration` (linhas 116-126), depois de `transcripts: meetingData.transcripts,`:

```ts
    canGenerate,
```

- [ ] **Step 7: Verificar tipos**

```bash
cd frontend && npx tsc --noEmit -p tsconfig.json
```

Esperado: nenhum erro novo. `Property 'canGenerate' is missing` indica que o Step 6 não foi aplicado.

- [ ] **Step 8: Commit**

```bash
git add frontend/src/hooks/meeting-details/useSummaryGeneration.ts frontend/src/app/meeting-details/page-content.tsx
git commit -m "feat(summary): gerar e regerar resumo sem transcricao"
```

---

### Task 6: Verificação manual no app

**Files:** nenhum arquivo alterado, salvo correções que a verificação exigir.

**Interfaces:**
- Consumes: tudo das Tasks 1-5.
- Produces: nada.

Esta task existe porque não há teste automatizado de frontend: é o único ponto onde o comportamento de ponta a ponta é confirmado. Não marcar a feature como concluída antes dela.

- [ ] **Step 1: Subir o app**

```bash
cd frontend && pnpm run tauri:dev
```

Requer `pnpm install` antes (ver Global Constraints).

- [ ] **Step 2: Reunião sem transcrição mostra o campo**

Abrir uma gravação feita pelo CLI sem transcrição. Esperado: painel esquerdo com a lista de transcrição vazia **e**, no rodapé, a barra de anexos + o textarea "Add context for AI summary...".

- [ ] **Step 3: Persistência**

Colar um texto no textarea, esperar ~1 s (debounce de 500 ms), navegar para outra reunião e voltar. Esperado: o texto continua lá.

- [ ] **Step 4: Anexo**

Arrastar um `.txt` ou `.md` para a região do textarea. Esperado: aparece um chip com o nome do arquivo. Se der o toast "Comece uma gravação antes de anexar arquivos a esta reunião", a reunião está sem `folder_path` — anotar qual comando do CLI a gerou e reportar; o `cli/runner.rs:877` deveria preencher.

- [ ] **Step 5: Gerar resumo só com contexto**

Com o texto colado e sem transcrição, clicar em gerar. Esperado: o botão está visível, a geração roda e sai um resumo baseado no texto colado. Nos logs do terminal (`RUST_LOG=info`), o prompt final deve conter `<no_transcript>` e não conter `<transcript_chunks>`.

- [ ] **Step 6: Regenerar**

Clicar em regerar o resumo recém-criado. Esperado: roda de novo (sem o `return` silencioso que existia antes da Task 5).

- [ ] **Step 7: Caso vazio**

Em uma reunião sem transcrição, sem contexto e sem anexo: o grupo de botões de geração fica oculto. Clicando em "Generate" pelo empty state, esperado: toast `Nada para resumir: adicione contexto ou um anexo`.

- [ ] **Step 8: Não-regressão**

Abrir uma reunião **com** transcrição. Esperado: campo de contexto, botões e geração exatamente como antes; o prompt nos logs volta a conter `<transcript_chunks>` e nenhum `<no_transcript>`.

- [ ] **Step 9: Commit de eventuais correções**

Se algum passo exigiu ajuste, commitar separadamente com mensagem descrevendo o defeito corrigido.

---

## Notas de escopo (decisões já tomadas, não reabrir)

- **Chunking do contexto:** o texto do contexto não passa por `rough_token_count`/`chunk_text` — só o parâmetro `text` passa. Limitação aceita (spec §7). Com Ollama/Built-in AI e um contexto muito longo, a janela pode estourar. Não tratar neste plano.
- **`EmptyStateSummary`** continua com botão de gerar não-gated. Com nada a resumir, o clique cai no guard da Task 5 e mostra o toast — resultado melhor que um botão sumido, e evita mais uma prop atravessando `SummaryPanel`.
- **Auto-geração** (`page-content.tsx:150`) continua exigindo `transcripts.length > 0`. É o fluxo de fim de gravação; não há motivo para disparar sozinha em reunião importada.
- **`save_transcript_data`** (`summary/commands.rs:211`) é chamado com `text` vazio nesse fluxo. Não há guard lá e não vamos adicionar um: o guard efetivo está em `generate_meeting_summary`. Se o Step 5 da Task 6 mostrar linha órfã em `transcript_chunks`, tratar como bug separado.
