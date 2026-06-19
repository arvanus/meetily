# CLI Panel — Largura Adaptativa Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminar o wrap/scroll infinito da linha de status do CLI fazendo o painel caber sempre em 1 linha física via layout adaptativo (descarte de campos por prioridade).

**Architecture:** `panel.rs` ganha `render_fit(state, max_cols)` puro que monta a linha a partir de campos ordenados por prioridade de descarte e remove campos até caber (fallback: trunca com `…`). `runner.rs` mede a largura do terminal (`terminal-size`) a cada tick e chama `render_fit`; como a linha nunca excede a largura, o terminal nunca faz wrap automático e `\r\x1b[K` limpa corretamente.

**Tech Stack:** Rust, crate `terminal-size`, testes `cargo test` (unitários puros em `panel.rs`).

## Global Constraints

- **NÃO executar `cargo check` / `cargo build` / `cargo test` em `frontend/src-tauri`** — o usuário compila e testa localmente. O worker escreve testes e código, commita, e pede ao usuário para rodar a verificação.
- `rust-version = "1.77"` — dependências novas devem respeitar esse MSRV.
- Glifos do painel (`● ○ ▁..█ ▮ ▯ │ ⚠ …`) são largura-1 → contagem por `chars().count()` representa colunas.
- Manter compatibilidade: `render(&s)` existente continua funcionando (vira `render_fit(&s, None)`); todos os testes atuais de `panel.rs` continuam válidos.

---

### Task 1: `render_fit` — layout adaptativo puro em `panel.rs`

**Files:**
- Modify: `frontend/src-tauri/src/cli/panel.rs` (função `render` em torno das linhas 46-75; testes no `mod tests`)

**Interfaces:**
- Consumes: `PanelState` (já existe), helpers `bar`, `meter`, `rms_to_level`, `hhmmss` (já existem).
- Produces:
  - `pub fn render(s: &PanelState) -> String` (mantida; delega para `render_fit(s, None)`)
  - `pub fn render_fit(s: &PanelState, max_cols: Option<usize>) -> String`
    - `None` → linha completa (comportamento atual).
    - `Some(w)` → descarta campos por prioridade até caber em `w` colunas; fallback trunca com `…`. Nunca contém `\n`. Resultado tem `chars().count() <= w` quando `w >= 1`.
    - Ordem de descarte (primeiro a sair → último): `model` → `tail` (segments/recorded) → `eq` → `meters`. Sempre presentes (salvo truncação final): `● REC hh:mm:ss` e `⚠ no audio for Ns` (quando ativo).

- [ ] **Step 1: Escrever os testes que falham**

Adicionar ao `mod tests` em `frontend/src-tauri/src/cli/panel.rs`:

```rust
    fn wide_state() -> PanelState {
        PanelState {
            elapsed_secs: 0,
            segments: 0,
            engine_model: "parakeet/parakeet-tdt-0.6b-v3-int8".to_string(),
            silent_secs: 82,
            ..Default::default()
        }
    }

    #[test]
    fn render_is_render_fit_none() {
        let s = wide_state();
        assert_eq!(render(&s), render_fit(&s, None));
    }

    #[test]
    fn fit_wide_keeps_everything() {
        let s = wide_state();
        let out = render_fit(&s, Some(200));
        assert!(out.contains("parakeet/parakeet-tdt-0.6b-v3-int8"));
        assert!(out.contains("segments:0"));
        assert!(out.contains("eq "));
        assert!(out.contains("mic "));
        assert!(out.contains("⚠ no audio for 82s"));
    }

    #[test]
    fn fit_drops_model_first() {
        // 90 colunas: cai só o model; tail e medidores permanecem.
        let s = wide_state();
        let out = render_fit(&s, Some(90));
        assert!(!out.contains("parakeet"));
        assert!(out.contains("segments:0"));
        assert!(out.contains("mic "));
        assert!(out.contains("⚠ no audio for 82s"));
        assert!(out.chars().count() <= 90);
    }

    #[test]
    fn fit_drops_segments_second() {
        // 70 colunas: caem model e tail; eq e medidores permanecem.
        let s = wide_state();
        let out = render_fit(&s, Some(70));
        assert!(!out.contains("parakeet"));
        assert!(!out.contains("segments"));
        assert!(out.contains("eq "));
        assert!(out.contains("mic "));
        assert!(out.chars().count() <= 70);
    }

    #[test]
    fn fit_drops_eq_third() {
        // 60 colunas: cai o eq; medidores ainda presentes.
        let s = wide_state();
        let out = render_fit(&s, Some(60));
        assert!(!out.contains("eq "));
        assert!(out.contains("mic "));
        assert!(out.chars().count() <= 60);
    }

    #[test]
    fn fit_drops_meters_last_warn_protected() {
        // 40 colunas: caem medidores; REC/tempo e aviso permanecem.
        let s = wide_state();
        let out = render_fit(&s, Some(40));
        assert!(!out.contains("mic "));
        assert!(out.contains("REC 00:00:00"));
        assert!(out.contains("⚠ no audio for 82s"));
        assert!(out.chars().count() <= 40);
    }

    #[test]
    fn fit_tiny_truncates_with_ellipsis_never_wraps() {
        let s = wide_state();
        let out = render_fit(&s, Some(20));
        assert!(out.chars().count() <= 20);
        assert!(out.ends_with('…'));
        assert!(!out.contains('\n'));
    }

    #[test]
    fn fit_zero_width_is_empty() {
        let s = wide_state();
        assert_eq!(render_fit(&s, Some(0)), "");
    }

    #[test]
    fn fit_record_only_tail_drops_in_segments_slot() {
        // record_only: o slot de prioridade 2 é "recorded:" (não "segments:").
        let s = PanelState { record_only: true, bytes_written: 2_097_152, engine_model: "m".into(), ..Default::default() };
        let wide = render_fit(&s, Some(200));
        assert!(wide.contains("recorded: 2.0 MB"));
        // largura média que derruba model e tail mas mantém medidores
        let narrow = render_fit(&s, Some(45));
        assert!(!narrow.contains("recorded:"));
        assert!(narrow.contains("mic "));
        assert!(narrow.chars().count() <= 45);
    }
```

- [ ] **Step 2: Verificar que falham (compilação)**

`render_fit` ainda não existe → não compila. (Não rode `cargo` — peça ao usuário se quiser confirmar; este passo só registra a expectativa: erro "cannot find function `render_fit`".)

- [ ] **Step 3: Implementar `render_fit` e converter `render`**

Substituir a função `render` atual (linhas ~45-75) por:

```rust
/// Renderiza UMA linha de status completa (sem newline). Atalho para
/// `render_fit(s, None)`. O chamador faz `\r` + flush.
pub fn render(s: &PanelState) -> String {
    render_fit(s, None)
}

/// Renderiza a linha de status cabendo em `max_cols` colunas (largura do
/// terminal). Quando estreito, descarta campos por prioridade para manter
/// SEMPRE 1 linha física (evita o wrap automático do terminal, que com
/// `\r\x1b[K` causava scroll infinito).
///
/// Ordem de descarte (primeiro a sair → último): model → tail → eq → meters.
/// Sempre presentes (salvo truncação final com `…`): `● REC hh:mm:ss` e o
/// aviso `⚠ no audio for Ns`. Com `max_cols = None`, devolve a linha completa.
pub fn render_fit(s: &PanelState, max_cols: Option<usize>) -> String {
    let dot = if s.pulse_on { '●' } else { '○' };
    let rec = format!("{} REC {}", dot, hhmmss(s.elapsed_secs));
    let eq = format!("eq {}{}{}", bar(s.bands[0]), bar(s.bands[1]), bar(s.bands[2]));
    let meters = format!(
        "mic {} sys {}",
        meter(rms_to_level(s.mic_rms), 4),
        meter(rms_to_level(s.system_rms), 4),
    );
    let tail = if s.record_only {
        format!(
            "recorded: {:.1} MB / {}",
            s.bytes_written as f64 / 1_048_576.0,
            hhmmss(s.elapsed_secs)
        )
    } else {
        format!("segments:{}", s.segments)
    };
    let warn = if s.silent_secs >= 5 {
        format!("⚠ no audio for {}s", s.silent_secs)
    } else {
        String::new()
    };

    // (texto, prioridade-de-descarte). 0 = essencial (só some na truncação
    // final). Maior número = descartado PRIMEIRO. Ordem visual = ordem do vec.
    let mut fields: Vec<(String, u8)> = vec![
        (rec, 0),
        (eq, 2),
        (meters, 1),
        (tail, 3),
        (s.engine_model.clone(), 4),
        (warn, 0),
    ];
    // engine_model/warn podem ser vazios (default/testes) — não exibir vazios.
    fields.retain(|(t, _)| !t.is_empty());

    let join = |fs: &[(String, u8)]| -> String {
        fs.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" │ ")
    };
    let cols = |t: &str| t.chars().count();

    let Some(max) = max_cols else {
        return join(&fields);
    };

    // Descarta o campo de MAIOR prioridade enquanto não couber e ainda houver
    // campos descartáveis (prioridade > 0).
    loop {
        if cols(&join(&fields)) <= max {
            return join(&fields);
        }
        let drop_idx = fields
            .iter()
            .enumerate()
            .filter(|(_, (_, p))| *p > 0)
            .max_by_key(|(_, (_, p))| *p)
            .map(|(i, _)| i);
        match drop_idx {
            Some(i) => {
                fields.remove(i);
            }
            None => break, // só restam essenciais
        }
    }

    // Fallback: só essenciais e ainda não cabe → trunca com '…'.
    let line = join(&fields);
    if cols(&line) <= max {
        return line;
    }
    if max == 0 {
        return String::new();
    }
    let keep = max.saturating_sub(1); // reserva 1 coluna para o '…'
    let mut out: String = line.chars().take(keep).collect();
    out.push('…');
    out
}
```

- [ ] **Step 4: Verificar que os testes passam**

Verificação feita pelo usuário (memória: não rodar cargo em `src-tauri`). Peça:
> "Rode `cargo test -p meetily cli::panel` (ou o teste do app_lib) e confirme que os 8 testes novos + os 5 antigos passam."
Expected: todos PASS.

- [ ] **Step 5: Commit**

```bash
git add frontend/src-tauri/src/cli/panel.rs
git commit -m "feat(cli): render_fit com layout adaptativo por largura

Descarta campos por prioridade (model -> segments -> eq -> meters)
mantendo REC/tempo e aviso de silencio. render() vira render_fit(_, None)."
```

---

### Task 2: Medir largura do terminal e usar `render_fit` no `runner.rs`

**Files:**
- Modify: `frontend/src-tauri/Cargo.toml:71` (`[dependencies]`)
- Modify: `frontend/src-tauri/src/cli/runner.rs:530-546` (draw tick)

**Interfaces:**
- Consumes: `panel::render_fit(&PanelState, Option<usize>)` da Task 1; `terminal_size::terminal_size()`.
- Produces: nenhuma API nova (efeito: a linha desenhada nunca excede a largura do terminal).

- [ ] **Step 1: Adicionar a dependência `terminal-size`**

Em `frontend/src-tauri/Cargo.toml`, dentro de `[dependencies]` (após a linha `chrono = ...` ~linha 89), adicionar:

```toml
# Largura do terminal para o painel CLI adaptativo (evita wrap/scroll infinito)
terminal-size = "0.4"
```

- [ ] **Step 2: Usar `render_fit` com a largura medida no draw tick**

Em `frontend/src-tauri/src/cli/runner.rs`, no bloco que monta `line` (linhas ~530-546), substituir:

```rust
                let line = {
                    let Ok(mut p) = panel.lock() else { continue };
                    let elapsed = started.elapsed().as_secs();
                    p.elapsed_secs = elapsed;
                    p.pulse_on = (elapsed % 2) == 0;
                    if record_only {
                        p.bytes_written = bytes;
                    }
                    // Silêncio: acumula 250ms por tick quando ambos rms < 0.01.
                    if p.mic_rms < 0.01 && p.system_rms < 0.01 {
                        silent_accum_ms = silent_accum_ms.saturating_add(250);
                    } else {
                        silent_accum_ms = 0;
                    }
                    p.silent_secs = silent_accum_ms / 1000;
                    crate::cli::panel::render(&p)
                };
```

por:

```rust
                // Largura do terminal por tick: sem TTY (pipe/redirect) -> None
                // (linha completa, não há terminal interativo para fazer wrap).
                let cols = terminal_size::terminal_size()
                    .map(|(terminal_size::Width(w), _)| w as usize);
                let line = {
                    let Ok(mut p) = panel.lock() else { continue };
                    let elapsed = started.elapsed().as_secs();
                    p.elapsed_secs = elapsed;
                    p.pulse_on = (elapsed % 2) == 0;
                    if record_only {
                        p.bytes_written = bytes;
                    }
                    // Silêncio: acumula 250ms por tick quando ambos rms < 0.01.
                    if p.mic_rms < 0.01 && p.system_rms < 0.01 {
                        silent_accum_ms = silent_accum_ms.saturating_add(250);
                    } else {
                        silent_accum_ms = 0;
                    }
                    p.silent_secs = silent_accum_ms / 1000;
                    crate::cli::panel::render_fit(&p, cols)
                };
```

- [ ] **Step 3: Verificação manual pelo usuário**

Verificação feita pelo usuário (memória: não rodar cargo em `src-tauri`). Peça:
> "Rode o CLI gravando (`meetily-cli`), encolha a janela do terminal abaixo da largura da linha e confirme: a linha permanece em 1 linha física (sem repetição/scroll), descartando model → segments → eq → medidores conforme estreita; `● REC` e o aviso de silêncio continuam visíveis. Em janela larga, a linha completa volta a aparecer."
Expected: sem scroll infinito; layout encolhe/expande com a janela.

- [ ] **Step 4: Commit**

```bash
git add frontend/src-tauri/Cargo.toml frontend/src-tauri/src/cli/runner.rs
git commit -m "fix(cli): painel cabe na largura do terminal (fim do wrap infinito)

Mede a largura via terminal-size por tick e usa render_fit; a linha nunca
excede a largura, entao \\r\\x1b[K limpa sempre 1 linha fisica."
```

---

## Self-Review

**Spec coverage:**
- Problema (wrap por `\x1b[K` limitado à linha física) → Task 2 garante linha ≤ largura. ✓
- Layout adaptativo, ordem model→segments→eq→meters → Task 1 `render_fit` + testes. ✓
- Essenciais protegidos (`● REC tempo`, `⚠ no audio`) → prioridade 0 + teste `fit_drops_meters_last_warn_protected`. ✓
- Fallback `…` em largura minúscula → Task 1 + teste `fit_tiny_truncates_with_ellipsis_never_wraps`. ✓
- Sem TTY → `None` → linha completa → Task 2 Step 2 (`terminal_size()` retorna `None`). ✓
- `record_only` no slot 2 → teste `fit_record_only_tail_drops_in_segments_slot`. ✓
- Dep `terminal-size` → Task 2 Step 1. ✓
- Fora de escopo (transcript 356/412) → não tocados. ✓

**Placeholder scan:** nenhum TBD/TODO; todo código presente. ✓

**Type consistency:** `render_fit(&PanelState, Option<usize>) -> String` usado igual na Task 1 (def) e Task 2 (chamada); `terminal_size::Width(w)` consistente. ✓
