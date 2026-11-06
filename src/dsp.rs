//! DSP: equalizador de 10 bandas (biquads RBJ) + pré-amp, e utilitários de ganho.

use std::f32::consts::PI;

pub const FREQS: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
pub const Q: f32 = 1.1;

pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqParams {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands: [f32; 10],
}

impl Default for EqParams {
    fn default() -> Self {
        Self { enabled: true, preamp_db: 0.0, bands: [0.0; 10] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    LowShelf,
    Peaking,
    HighShelf,
}

#[derive(Clone, Copy, Default)]
struct Coef {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

#[derive(Clone, Copy, Default)]
struct State {
    z1: f32,
    z2: f32,
}

fn design(kind: Kind, freq: f32, gain_db: f32, sample_rate: f32) -> Coef {
    // Cookbook de Robert Bristow-Johnson.
    let a = 10f32.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * (freq.min(sample_rate * 0.49)) / sample_rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * Q);
    let (b0, b1, b2, a0, a1, a2) = match kind {
        Kind::Peaking => (
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        ),
        Kind::LowShelf => {
            let t = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cos + t),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - t),
                (a + 1.0) + (a - 1.0) * cos + t,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - t,
            )
        }
        Kind::HighShelf => {
            let t = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cos + t),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - t),
                (a + 1.0) - (a - 1.0) * cos + t,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - t,
            )
        }
    };
    Coef { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// Equalizador para áudio intercalado com `channels` canais.
pub struct Equalizer {
    sample_rate: f32,
    channels: usize,
    params: EqParams,
    coefs: [Coef; 10],
    states: Vec<[State; 10]>,
    pre: f32,
}

impl Equalizer {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let mut eq = Self {
            sample_rate: sample_rate as f32,
            channels,
            params: EqParams::default(),
            coefs: [Coef::default(); 10],
            states: vec![[State::default(); 10]; channels],
            pre: 1.0,
        };
        eq.set_params(EqParams::default());
        eq
    }

    pub fn set_params(&mut self, p: EqParams) {
        self.params = p;
        for i in 0..10 {
            let kind = match i {
                0 => Kind::LowShelf,
                9 => Kind::HighShelf,
                _ => Kind::Peaking,
            };
            let g = if p.enabled { p.bands[i] } else { 0.0 };
            self.coefs[i] = design(kind, FREQS[i], g, self.sample_rate);
        }
        self.pre = db_to_lin(if p.enabled { p.preamp_db } else { 0.0 });
    }

    /// Processa um bloco intercalado, in-place.
    pub fn process(&mut self, buf: &mut [f32]) {
        let ch = self.channels;
        // Bandas com ganho 0 dB são transparentes: pula para economizar CPU.
        let active: Vec<usize> = (0..10)
            .filter(|&i| self.params.enabled && self.params.bands[i].abs() > 1e-3)
            .collect();
        for frame in buf.chunks_exact_mut(ch) {
            for (c, s) in frame.iter_mut().enumerate() {
                let mut x = *s * self.pre;
                for &i in &active {
                    let k = self.coefs[i];
                    let st = &mut self.states[c][i];
                    // forma transposta II
                    let y = k.b0 * x + st.z1;
                    st.z1 = k.b1 * x - k.a1 * y + st.z2;
                    st.z2 = k.b2 * x - k.a2 * y;
                    x = y;
                }
                *s = x;
            }
        }
    }
}

/// Ganho linear do ReplayGain para uma faixa. `None` em `gain` = faixa sem tag.
/// Retorna (dB aplicados, nota de clipping).
pub fn replaygain_db(mode_album: bool, gains: (Option<f32>, Option<f32>), peaks: (Option<f32>, Option<f32>),
                     preamp: f32, prevent_clip: bool) -> Option<f32> {
    let (tg, ag) = gains;
    let (tp, ap) = peaks;
    let gain = if mode_album { ag.or(tg) } else { tg.or(ag) };
    let peak = if mode_album { ap.or(tp) } else { tp.or(ap) };
    let gain = gain?;
    let mut db = gain + preamp;
    if prevent_clip {
        if let Some(p) = peak {
            if p > 0.0 && db + 20.0 * p.log10() > 0.0 {
                db = -20.0 * p.log10();
            }
        }
    }
    Some(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * freq * i as f32 / sr).sin() * 0.5).collect()
    }

    #[test]
    fn flat_eq_is_transparent() {
        let mut eq = Equalizer::new(48000, 1);
        let mut x = sine(1000.0, 48000.0, 4800);
        let y = x.clone();
        eq.process(&mut x);
        assert_eq!(x, y);
    }

    #[test]
    fn peaking_band_boosts_its_frequency() {
        let mut eq = Equalizer::new(48000, 1);
        let mut p = EqParams::default();
        p.bands[5] = 6.0; // 1 kHz, +6 dB
        eq.set_params(p);
        let mut x = sine(1000.0, 48000.0, 48000);
        let before = rms(&x[24000..]);
        eq.process(&mut x);
        let gain_db = 20.0 * (rms(&x[24000..]) / before).log10();
        assert!((gain_db - 6.0).abs() < 0.3, "ganho {gain_db}");
    }

    #[test]
    fn low_shelf_leaves_highs_alone() {
        let mut eq = Equalizer::new(48000, 1);
        let mut p = EqParams::default();
        p.bands[0] = 9.0;
        eq.set_params(p);
        let mut x = sine(8000.0, 48000.0, 48000);
        let before = rms(&x[24000..]);
        eq.process(&mut x);
        let gain_db = 20.0 * (rms(&x[24000..]) / before).log10();
        assert!(gain_db.abs() < 0.5, "ganho {gain_db}");
    }

    #[test]
    fn preamp_scales() {
        let mut eq = Equalizer::new(44100, 2);
        let mut p = EqParams::default();
        p.preamp_db = -6.0;
        eq.set_params(p);
        let mut x = vec![0.5f32; 100];
        eq.process(&mut x);
        assert!((x[0] - 0.5 * db_to_lin(-6.0)).abs() < 1e-6);
    }

    #[test]
    fn replaygain_prevents_clipping() {
        // +6 dB com pico 1.0 deve ser limitado a 0 dB
        let db = replaygain_db(false, (Some(6.0), None), (Some(1.0), None), 0.0, true).unwrap();
        assert!(db.abs() < 1e-4);
        let db = replaygain_db(false, (Some(6.0), None), (Some(1.0), None), 0.0, false).unwrap();
        assert!((db - 6.0).abs() < 1e-4);
        assert!(replaygain_db(true, (None, None), (None, None), 0.0, true).is_none());
    }
}
