# CLI Panel — Largura Adaptativa (fix wrap/scroll infinito)

**Data:** 2026-06-19
**Branch:** test/all-features
**Arquivos:** `frontend/src-tauri/src/cli/panel.rs`, `frontend/src-tauri/src/cli/runner.rs`, `frontend/src-tauri/Cargo.toml`

## Problema

A linha de status viva do CLI é renderizada por `panel::render` como UMA linha larga e
desenhada em `runner.rs` com `print!("\r\x1b[K{line}")`. A sequência `\x1b[K` limpa apenas
a linha física atual (do cursor até o fim).

Quando a janela do terminal fica mais estreita que a largura da linha, o terminal faz
**wrap automático** da linha para 2+ linhas físicas. No tick seguinte, `\r` retorna o cursor
apenas ao início da última linha física; as linhas físicas anteriores não são limpas e
permanecem na tela. Cada tick (a cada ~250ms) acrescenta novo wrap → a linha "se perde" e
fica repetindo / rolando ad-infinitum.

Reprodução: rodar o CLI gravando, encolher a largura da janela abaixo do comprimento da
linha (ex.: `● REC 00:01:22 │ eq ▁▁▁ │ mic ▯▯▯▯ sys ▯▯▯▯ │ segments:0 │ parakeet/parakeet-tdt-0.6b-v3-int8  ⚠ no audio for 82s`).

## Decisão

Manter SEMPRE 1 linha física. Em vez de truncar cega, usar **layout adaptativo**: medir a
largura do terminal e descartar campos por prioridade até caber, mantendo o essencial legível.

Wrap multi-linha foi descartado (rouba linhas da rolagem, mais frágil). Truncação cega
simples foi descartada em favor do descarte por prioridade.

## Ordem de descarte (do mais descartável ao mais essencial)

À medida que a largura disponível encolhe, remover na ordem:

1. **model** — `engine_model` (ex.: `parakeet/parakeet-tdt-0.6b-v3-int8`)
2. **segments** — `segments:N` (ou, em `record_only`, `recorded: X MB / hh:mm:ss`)
3. **eq** — `eq ▁▁▁`
4. **medidores** — `mic ▮▯▯▯ sys ▯▯▯▯`
5. **fallback final** — se ainda não couber, truncar o que restar com `…`

**Sempre visível** (intocável, exceto pelo fallback `…` extremo):
- `● REC hh:mm:ss`
- `⚠ no audio for Ns` (quando `silent_secs >= 5`) — é o sintoma do próprio problema de áudio,
  fica protegido.

Separador `│` é inserido apenas entre os campos efetivamente presentes (sem `│` órfão nas
pontas nem duplo `│` quando um campo do meio é removido).

## Componentes

### `panel.rs` (lógica pura, testável)

- Refatorar `render` para construir a linha a partir de uma lista ordenada de **segmentos**
  (cada um com seu nível de prioridade), em vez de um único `format!`.
- Nova função pública `render_fit(s: &PanelState, max_cols: Option<usize>) -> String`:
  - `max_cols == None` → comportamento atual (linha completa, sem corte).
  - `max_cols == Some(w)` → monta a linha; enquanto exceder `w` colunas, remove o próximo
    campo descartável na ordem acima; junta os campos restantes com ` │ ` apenas entre
    presentes; se ainda exceder, trunca com `…`.
- `render(s)` existente passa a ser `render_fit(s, None)` (compatibilidade + testes atuais
  continuam válidos).
- Contagem de colunas: os glifos usados (`● ○ ▁..█ ▮ ▯ │ ⚠ … :` e dígitos/letras ASCII) são
  todos largura-1 em terminais comuns → contar `chars().count()` é suficiente. Reservar 1
  coluna para o `…` no fallback.

### `runner.rs` (draw tick, ~linhas 530-548)

- Obter a largura do terminal via `terminal_size()` no tick.
- Chamar `panel::render_fit(&p, width)` onde `width = Some(cols)` se houver TTY, senão `None`.
- Manter `print!("\r\x1b[K{line}")` — agora a linha nunca excede a largura, então nunca há
  wrap automático.

### `Cargo.toml`

- Adicionar dependência `terminal-size` (crate pequeno, cross-platform Windows/Unix; retorna
  `Option<(Width, Height)>`).

## Fallback / casos de borda

- **Sem TTY** (saída redirecionada/pipe): `terminal_size()` retorna `None` → `render_fit`
  usa `None` → linha completa. Sem terminal interativo não há wrap-em-loop.
- **Largura minúscula** (ex.: < comprimento de `● REC 00:00:00`): após descartar tudo,
  o fallback trunca o essencial com `…`. Garante 1 linha sempre.
- **`record_only`**: o campo de prioridade 2 é o `recorded: X MB / hh:mm:ss` em vez de
  `segments:N` (mesma posição na ordem).

## Fora de escopo

- Linhas de transcript (`runner.rs:356`, `:412`): a 356 usa `\n` (rola normalmente); a 412 é
  transitória. Não causam o loop infinito — não alteradas.
- Reflow/redimensionamento "bonito" (re-render em evento de resize): o tick de ~250ms já
  re-renderiza com a nova largura; não há listener de resize dedicado.

## Testes (em `panel.rs`)

- Testes atuais continuam passando (`render` = `render_fit(_, None)`).
- `render_fit` com largura ampla == `render` completo.
- Largura média: cai o `model` primeiro; `segments` e medidores presentes.
- Largura menor: caem `model` e `segments`; sem `│` órfão.
- Largura pequena: caem até medidores; `● REC tempo` e `⚠ no audio` presentes.
- Largura minúscula: fallback `…`, nunca excede a largura, nunca contém `\n`.
- `record_only`: campo de prioridade 2 é `recorded:` e cai junto com a posição de segments.
