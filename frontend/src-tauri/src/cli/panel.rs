//! Render da linha de status viva do CLI. Mantemos o render PURO (string a partir
//! de um struct) para poder testar; o desenho no terminal usa `\r` + ANSI no runner.

/// Estado renderizável do painel (alimentado por audio-levels + contadores).
#[derive(Debug, Clone, Default)]
pub struct PanelState {
    pub elapsed_secs: u64,
    pub mic_rms: f32,
    pub system_rms: f32,
    pub bands: [f32; 3], // graves, médios, agudos (0.0..=1.0) — preenchido na Task 5
    pub segments: u64,
    pub engine_model: String,
    pub record_only: bool,
    pub bytes_written: u64,
    pub silent_secs: u64,
    pub pulse_on: bool,
    /// Gravação pausada (tecla 'p'): troca `● REC` por `⏸ PAUSED` e o runner
    /// congela o tempo decorrido enquanto verdadeiro.
    pub paused: bool,
}

fn bar(level: f32) -> char {
    let blocks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let idx = (level.clamp(0.0, 1.0) * (blocks.len() as f32 - 1.0)).round() as usize;
    blocks[idx]
}

fn meter(level: f32, width: usize) -> String {
    let filled = (level.clamp(0.0, 1.0) * width as f32).round() as usize;
    (0..width).map(|i| if i < filled { '▮' } else { '▯' }).collect()
}

/// Converte RMS linear em nível perceptual 0..1 (escala dB). Fala normal fica em
/// RMS ~0.02-0.15, que num medidor linear mal sai do primeiro bloco; mapeamos
/// -50 dBFS (silêncio útil) até 0 dBFS (full scale) para encher o medidor.
fn rms_to_level(rms: f32) -> f32 {
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 50.0) / 50.0).clamp(0.0, 1.0)
}

fn hhmmss(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

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
    let rec = if s.paused {
        format!("⏸ PAUSED {}", hhmmss(s.elapsed_secs))
    } else {
        let dot = if s.pulse_on { '●' } else { '○' };
        format!("{} REC {}", dot, hhmmss(s.elapsed_secs))
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_only_shows_bytes_not_segments() {
        let s = PanelState { record_only: true, bytes_written: 2_097_152, elapsed_secs: 65, ..Default::default() };
        let out = render(&s);
        assert!(out.contains("recorded: 2.0 MB / 00:01:05"));
        assert!(!out.contains("segments:"));
    }

    #[test]
    fn normal_shows_segments() {
        let s = PanelState { segments: 12, ..Default::default() };
        assert!(render(&s).contains("segments:12"));
    }

    #[test]
    fn silence_warning_after_5s() {
        let s = PanelState { silent_secs: 6, ..Default::default() };
        assert!(render(&s).contains("⚠ no audio for 6s"));
    }

    #[test]
    fn meter_is_perceptual_not_linear() {
        // Fala normal (RMS ~0.05 ≈ -26 dBFS) precisa encher ~metade do medidor,
        // não ficar presa no primeiro bloco como na escala linear.
        assert_eq!(rms_to_level(0.0), 0.0);
        assert!(rms_to_level(0.003) < 0.05); // ruído de fundo ≈ vazio
        let speech = rms_to_level(0.05);
        assert!(speech > 0.4 && speech < 0.6, "speech level = {speech}");
        assert_eq!(rms_to_level(1.0), 1.0); // full scale enche tudo
    }

    #[test]
    fn paused_shows_pause_state_not_rec() {
        let s = PanelState { paused: true, elapsed_secs: 83, ..Default::default() };
        let out = render(&s);
        assert!(out.contains("⏸ PAUSED 00:01:23"));
        assert!(!out.contains("REC"));
    }

    #[test]
    fn pulse_toggles_dot() {
        let on = PanelState { pulse_on: true, ..Default::default() };
        let off = PanelState { pulse_on: false, ..Default::default() };
        assert!(render(&on).starts_with('●'));
        assert!(render(&off).starts_with('○'));
    }

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
        // record_only: o 2º campo descartado (slot de segments) é "recorded:".
        let s = PanelState { record_only: true, bytes_written: 2_097_152, engine_model: "m".into(), ..Default::default() };
        let wide = render_fit(&s, Some(200));
        assert!(wide.contains("recorded: 2.0 MB"));
        // largura média que derruba model e tail mas mantém medidores
        let narrow = render_fit(&s, Some(45));
        assert!(!narrow.contains("recorded:"));
        assert!(narrow.contains("mic "));
        assert!(narrow.chars().count() <= 45);
    }
}
