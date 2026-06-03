//! FFT de 3 bandas (graves/médios/agudos) para o equalizador do painel do CLI.
use realfft::RealFftPlanner;

/// Calcula 3 magnitudes de banda (graves/médios/agudos), normalizadas 0..=1,
/// a partir de uma janela de amostras mono e da sample rate.
pub fn three_bands(samples: &[f32], sample_rate: u32) -> [f32; 3] {
    let n = samples.len();
    if n < 16 {
        return [0.0; 3];
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(n);
    let mut input = samples.to_vec();
    let mut spectrum = r2c.make_output_vec();
    if r2c.process(&mut input, &mut spectrum).is_err() {
        return [0.0; 3];
    }

    let bin_hz = sample_rate as f32 / n as f32;
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
        if counts[i] > 0 {
            bands[i] /= counts[i] as f32;
        }
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
        let b = three_bands(&sine(100.0, 16000, 1024), 16000);
        assert!(b[0] > b[1] && b[0] > b[2], "graves deveriam dominar: {b:?}");
    }

    #[test]
    fn high_tone_dominates_treble_band() {
        let b = three_bands(&sine(6000.0, 16000, 1024), 16000);
        assert!(b[2] > b[0], "agudos deveriam dominar graves: {b:?}");
    }

    #[test]
    fn silence_is_zero() {
        assert_eq!(three_bands(&vec![0.0; 1024], 16000), [0.0, 0.0, 0.0]);
    }
}
