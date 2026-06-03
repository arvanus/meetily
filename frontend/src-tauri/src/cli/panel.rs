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

fn hhmmss(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Renderiza UMA linha de status (sem newline). O chamador faz `\r` + flush.
pub fn render(s: &PanelState) -> String {
    let dot = if s.pulse_on { '●' } else { '○' };
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
        format!("  ⚠ no audio for {}s", s.silent_secs)
    } else {
        String::new()
    };
    format!(
        "{dot} REC {time}  │ bass {g} mid {m} treble {a} │ mic {mic}  sys {sys} │ {tail} │ {em}{warn}",
        dot = dot,
        time = hhmmss(s.elapsed_secs),
        g = bar(s.bands[0]),
        m = bar(s.bands[1]),
        a = bar(s.bands[2]),
        mic = meter(s.mic_rms, 4),
        sys = meter(s.system_rms, 4),
        tail = tail,
        em = s.engine_model,
        warn = warn,
    )
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
    fn pulse_toggles_dot() {
        let on = PanelState { pulse_on: true, ..Default::default() };
        let off = PanelState { pulse_on: false, ..Default::default() };
        assert!(render(&on).starts_with('●'));
        assert!(render(&off).starts_with('○'));
    }
}
