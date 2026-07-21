# Contexto de resumo em reuniões sem transcrição

**Data:** 2026-07-21
**Status:** Draft
**Autor:** Lucas Rubian Schatz
**Relacionado:** [2026-05-22-meeting-summary-context-design.md](2026-05-22-meeting-summary-context-design.md)

## 1. Problema

Uma gravação criada pelo CLI sem transcrição (ex.: `record` sem whisper, ou
uma conversa longa cuja transcrição falhou) abre na UI **sem o campo de
contexto**. O usuário quer colar nesse campo uma transcrição obtida em outro
serviço e gerar o resumo a partir dela.

Hoje isso é impossível por quatro travas independentes:

| # | Local | Trava |
|---|---|---|
| 1 | `frontend/src/components/MeetingDetails/TranscriptPanel.tsx:162` | `!isRecording && convertedSegments.length > 0` esconde o textarea **e** a barra de anexos |
| 2 | `frontend/src/components/MeetingDetails/SummaryGeneratorButtonGroup.tsx:76` | `if (!hasTranscripts) return null` remove o botão de gerar |
| 3 | `frontend/src/hooks/meeting-details/useSummaryGeneration.ts:411` | aborta com `'No transcripts available for summary'` |
| 4 | `frontend/src/hooks/meeting-details/useSummaryGeneration.ts:72` | lança `'No transcript text available. Please add some text first.'` |

No Rust não há trava: `processor.rs:190` apenas conta tokens de `text`; texto
vazio segue pelo caminho single-pass sem erro.

## 2. Objetivos

- Exibir o campo de contexto e a barra de anexos em reuniões sem transcrição.
- Permitir gerar resumo quando houver contexto e/ou anexos, mesmo sem transcrição.
- Informar explicitamente ao LLM que não há transcrição, para que ele se baseie
  em contexto + anexos.
- Abortar com erro claro quando não houver transcrição, contexto **nem** anexos.

## 3. Não-objetivos

- **Transformar o texto colado em transcrição.** Decisão explícita do usuário:
  o texto é contexto. Nada é gravado em `transcripts`; o painel de transcrição
  continua vazio, e "Copy Transcript" segue desabilitado.
- **Chunking do contexto.** O texto do contexto não passa por
  `rough_token_count`/`chunk_text` (apenas o parâmetro `text` passa). Ver §7.
- Importar transcrição de arquivo (`.srt`, `.vtt`, `.txt`). Candidato a v2.
- Mexer em `meeting_notes` (feature de observações durante a gravação — commit
  `575abc7`). Notas não são injetadas no prompt e não aparecem em
  meeting-details; são um eixo separado.

## 4. Comportamento

Ao abrir uma gravação sem transcrição:

1. O painel de contexto (chips de anexo + textarea) aparece normalmente,
   no mesmo lugar de sempre — rodapé do painel de transcrição.
2. O usuário cola o texto. Autossalva com o debounce de 500 ms já existente
   (`useSummaryContext`).
3. O botão de gerar resumo fica habilitado assim que houver contexto não-vazio
   ou pelo menos um anexo.
4. O resumo é gerado a partir de contexto + anexos.
5. Recarregar a página preserva texto e chips (persistência já implementada).

Sem transcrição, sem contexto e sem anexos: o botão continua oculto, e uma
chamada direta ao backend retorna erro (guarda em §5.3).

## 5. Mudanças

### 5.1 Exibição do painel de contexto

`TranscriptPanel.tsx:162` — a condição passa de:

```tsx
{!isRecording && convertedSegments.length > 0 && (
```

para:

```tsx
{!isRecording && (
```

A contagem de segmentos não tem relação com a utilidade do campo. Durante a
gravação ele continua oculto (comportamento atual preservado).

### 5.2 Gate de geração

O predicado deixa de ser "tem transcrição" e passa a ser "tem alguma fonte de
conteúdo":

```ts
canGenerateSummary =
  transcripts.length > 0 ||
  contextPrompt.trim() !== '' ||
  attachments.length > 0
```

| Arquivo | Mudança |
|---|---|
| `SummaryGeneratorButtonGroup.tsx:37,52,76` | prop `hasTranscripts?: boolean` → `canGenerateSummary?: boolean` (mesmo default `true`, mesmo `return null`) |
| `SummaryPanel.tsx:115,154,181` | deixa de computar `hasTranscripts={transcripts.length > 0}`; passa adiante `canGenerateSummary` recebido via prop |
| `app/meeting-details/page-content.tsx` | computa `canGenerateSummary` e passa a `SummaryPanel` |

`page-content.tsx` é o único ponto que tem os três valores à mão: `transcripts`
(estado da página) e `summaryContext.contextPrompt` /
`summaryContext.attachments` (linhas 179-181). Alternativa rejeitada: passar
`attachments` para dentro de `SummaryPanel` só para recomputar lá — aumenta a
superfície de props de um componente já grande, sem ganho.

### 5.3 Hook de geração

`useSummaryGeneration` recebe um objeto de props (`useSummaryGeneration.ts:25`);
ganha mais uma: `canGenerate: boolean` — o **mesmo** valor de §5.2, computado
uma vez em `page-content.tsx` e distribuído para o hook e para `SummaryPanel`.
Um único booleano em vez de um `hasContextContent` separado: o hook precisa
exatamente da pergunta "existe alguma fonte de conteúdo?", e duplicar a regra
nos dois lugares abriria espaço para divergirem. Com isso:

- **linha 411:** com zero transcrições, seguir adiante com `transcriptText = ''`
  se houver contexto ou anexo. Só aborta (toast + `return`) se as três fontes
  estiverem vazias. Mensagem: `'Nada para resumir: adicione contexto ou um anexo'`.
- **linha 72:** o `throw` em `!transcriptText.trim()` deixa de ser incondicional
  pela mesma razão. A validação real de "há conteúdo" fica no ponto acima e no
  guard do Rust.
- `setOriginalTranscript('')` no caminho sem transcrição é aceitável, mas
  obriga a afrouxar também **a linha 579** (`handleRegenerateSummary`), que hoje
  retorna sem fazer nada quando `originalTranscript` está vazio. Sem isso, um
  resumo gerado só a partir do contexto nunca poderia ser regerado.

### 5.4 Prompt (Rust)

`summary/processor.rs` (~linha 357). Hoje:

```rust
final_user_prompt.push_str(&format!(
    r#"<transcript_chunks>
{}
</transcript_chunks>"#,
    content_to_summarize
));
```

Passa a emitir, quando `content_to_summarize` estiver vazio (após `trim`), um
marcador no lugar do bloco:

```
<no_transcript>
This meeting has no transcript. Base the summary entirely on the
user-provided context and attachments below.
</no_transcript>
```

Com transcrição, o prompt é byte-idêntico ao atual. Os blocos `<attachments>` e
`<user_context>` que vêm em seguida permanecem inalterados nos dois casos.

### 5.5 Guarda no backend

`generate_meeting_summary` (`processor.rs:160`) retorna
`Err("Nothing to summarize: transcript, context and attachments are all empty")`
quando `text.trim()`, `context_prompt.trim()` e `attachments` estão todos
vazios. Não depender apenas da checagem do front — a regeneração automática
(`page-content.tsx:150`) e chamadas diretas ao comando Tauri contornam a UI.

### 5.6 Persistência

Nenhuma mudança de schema. Salvar o texto do contexto é DB-puro
(`meeting_summary_context`, PK `meeting_id`) e não depende de
`meetings.folder_path`. **Anexos** exigem `folder_path` (spec de 2026-05-22,
§5, passo 5). O fluxo do CLI **popula** `folder_path`: `cli/runner.rs:877` grava
a reunião com `folder_path` preenchido justamente para o app oferecer
re-transcrição. Portanto anexos funcionam em gravações do CLI. Se vier `NULL`
por outro caminho, o toast existente aparece ("Comece uma gravação antes de
anexar arquivos a esta reunião") — comportamento atual, sem mudança.

## 6. Erros

| Cenário | Comportamento |
|---|---|
| Sem transcrição, sem contexto, sem anexo | Botão oculto na UI; backend retorna erro (§5.5) |
| Sem transcrição, com contexto | Gera normalmente com `<no_transcript>` |
| Sem transcrição, só com anexo (contexto vazio) | Gera normalmente; `<user_context>` é omitido, como já acontece hoje |
| Anexo com arquivo sumido no disco | Comportamento atual: pula o anexo, loga warning, continua |

## 7. Limitação conhecida (aceita)

O texto do contexto **não passa por chunking**. Apenas o parâmetro `text` é
medido (`processor.rs:190`) e fatiado (`processor.rs:213`). Colar uma
transcrição longa no contexto e usar Ollama ou Built-in AI pode estourar a
janela do modelo, com falha ou truncamento do lado do provider.

Decisão do usuário: aceitar. Provedores cloud (Claude, Groq, OpenRouter,
CustomOpenAI) não sofrem, porque já usam single-pass independente do tamanho
(`processor.rs:199`).

Candidatos a v2: contar tokens do contexto e avisar na UI; ou rotear o contexto
para o slot `text` quando não há transcrição, ganhando o map-reduce.

## 8. Testes

### Rust

- `processor.rs`: `text` vazio → prompt contém `<no_transcript>` e **não**
  contém `<transcript_chunks>`.
- `processor.rs`: `text` não-vazio → prompt inalterado (sem `<no_transcript>`).
- `processor.rs`: as três fontes vazias → `Err` (§5.5).
- `processor.rs`: `text` vazio + anexo presente → bloco `<attachments>` continua
  sendo renderizado.

### Frontend

O projeto **não tem runner de testes** — nenhum arquivo `.test.tsx`/`.spec.ts`
versionado, nenhum config de vitest/jest, e `package.json` sem script `test`.
Decisão: não introduzir essa infra nesta feature. A validação de frontend é
`npx tsc --noEmit` mais a checklist manual abaixo.

A regra de negócio fica em um módulo puro (`lib/summary-generation.ts`),
espelhando `has_summarizable_content` do Rust — que **é** coberto por
`cargo test`. Adicionar vitest e testar `canGenerateSummary` diretamente
continua sendo um bom próximo passo, fora deste escopo.

### Manual

1. Gravar pelo CLI sem transcrição; abrir na UI → campo de contexto visível.
2. Colar texto → recarregar a página → texto ainda lá.
3. Gerar resumo → sai um resumo baseado no texto colado.
4. Reunião com transcrição → nada mudou (campo, botão e resumo iguais).

## 9. Rollout

Puramente aditivo. Sem migração, sem feature flag. Reuniões existentes com
transcrição não mudam de comportamento em nenhum ponto.
