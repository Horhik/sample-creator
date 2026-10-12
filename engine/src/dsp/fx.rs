//! A region's volume and dynamics (see [`crate::core::Fx`]), in this order:
//! trim silence, noise reduction, compressor, then normalise or gain.

use nnnoiseless::DenoiseState;

use crate::core::Fx;

pub fn apply(samples: Vec<f32>, channels: usize, rate: u32, fx: &Fx) -> Vec<f32> {
    if fx.is_neutral() || samples.is_empty() {
        return samples;
    }
    let mut s = samples;
    if let Some(db) = fx.trim_db {
        s = trim(&s, channels, rate, f32::from(db));
    }
    if fx.denoise {
        s = denoise(&s, channels, rate);
    }
    if fx.compress > 0 {
        compress(&mut s, channels, rate, fx.compress);
    }
    let gain = if fx.normalize {
        let peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
        // −1 dBFS: loud, with a little room for converters.
        if peak > 1e-6 { 0.891 / peak } else { 1.0 }
    } else {
        fx.gain_factor()
    };
    if (gain - 1.0).abs() > 1e-6 {
        s.iter_mut().for_each(|v| *v *= gain);
    }
    s
}

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Cuts the quiet below `threshold_db` at both ends, keeping 10 ms before
/// and after the sound and fading the new edges over 3 ms so nothing clicks.
fn trim(s: &[f32], channels: usize, rate: u32, threshold_db: f32) -> Vec<f32> {
    let frames = s.len() / channels;
    let thr = db_to_linear(threshold_db);
    let loud = |f: usize| {
        s[f * channels..(f + 1) * channels]
            .iter()
            .any(|v| v.abs() > thr)
    };
    let Some(first) = (0..frames).find(|&f| loud(f)) else {
        return s.to_vec(); // all quiet: leave it, an empty sample helps nobody
    };
    let last = (0..frames).rev().find(|&f| loud(f)).unwrap_or(first);
    let margin = (rate as usize) / 100;
    let a = first.saturating_sub(margin);
    let b = (last + 1 + margin).min(frames);
    let mut out = s[a * channels..b * channels].to_vec();
    let fade = ((rate as usize) * 3 / 1000).min((b - a) / 2);
    let n = b - a;
    for i in 0..fade {
        let g = i as f32 / fade as f32;
        for c in 0..channels {
            if a > 0 {
                out[i * channels + c] *= g;
            }
            if b < frames {
                out[(n - 1 - i) * channels + c] *= g;
            }
        }
    }
    out
}

/// Speech noise reduction (RNNoise). It works at 48 kHz in 10 ms frames,
/// so other rates go there and back through the band-limited resampler.
fn denoise(s: &[f32], channels: usize, rate: u32) -> Vec<f32> {
    let frames = s.len() / channels;
    let mut out = vec![0f32; s.len()];
    for c in 0..channels {
        let mono: Vec<f32> = (0..frames).map(|f| s[f * channels + c]).collect();
        let at48 = if rate == 48_000 {
            mono
        } else {
            super::speed::change_speed(&mono, 1, rate, f64::from(rate) / 48_000.0, false)
        };
        let mut state = DenoiseState::new();
        let n = DenoiseState::FRAME_SIZE;
        let mut clean = Vec::with_capacity(at48.len() + n);
        let mut input = vec![0f32; n];
        let mut output = vec![0f32; n];
        // RNNoise answers one frame late; feed one extra frame and drop the first.
        for chunk in 0..at48.len().div_ceil(n) + 1 {
            for (i, x) in input.iter_mut().enumerate() {
                // It expects samples on the 16-bit scale.
                *x = at48.get(chunk * n + i).copied().unwrap_or(0.0) * 32768.0;
            }
            state.process_frame(&mut output, &input);
            if chunk > 0 {
                clean.extend(output.iter().map(|v| v / 32768.0));
            }
        }
        clean.truncate(at48.len());
        let back = if rate == 48_000 {
            clean
        } else {
            super::speed::change_speed(&clean, 1, rate, 48_000.0 / f64::from(rate), false)
        };
        for f in 0..frames {
            out[f * channels + c] = back.get(f).copied().unwrap_or(0.0);
        }
    }
    out
}

/// A plain feed-forward compressor, stereo-linked. `amount` (1–100) lowers
/// the threshold to −30 dB and raises the ratio to 5:1; half of the
/// reduction at the threshold comes back as make-up gain.
fn compress(s: &mut [f32], channels: usize, rate: u32, amount: u8) {
    let a = f32::from(amount.min(100)) / 100.0;
    let threshold_db = -30.0 * a;
    let ratio = 1.0 + 4.0 * a;
    let makeup_db = -threshold_db * (1.0 - 1.0 / ratio) * 0.5;
    let coef = |ms: f32| (-1.0 / (ms / 1000.0 * rate as f32)).exp();
    let (attack, release) = (coef(5.0), coef(120.0));
    let mut env_db = -120.0f32;
    for frame in s.chunks_mut(channels) {
        let level = frame.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
        let level_db = 20.0 * level.log10();
        let c = if level_db > env_db { attack } else { release };
        env_db = c * env_db + (1.0 - c) * level_db;
        let over = (env_db - threshold_db).max(0.0);
        let gain_db = -over * (1.0 - 1.0 / ratio) + makeup_db;
        let g = db_to_linear(gain_db);
        frame.iter_mut().for_each(|v| *v *= g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(RATE as f32 * secs) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * amp)
            .collect()
    }

    #[test]
    fn neutral_changes_nothing() {
        let x = tone(0.1, 0.5);
        assert_eq!(apply(x.clone(), 1, RATE, &Fx::default()), x);
    }

    #[test]
    fn gain_and_normalize() {
        let x = tone(0.1, 0.25);
        let louder = apply(
            x.clone(),
            1,
            RATE,
            &Fx {
                gain: 60,
                ..Fx::default()
            },
        );
        let peak = louder.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.25 * 1.995).abs() < 0.01, "{peak}");
        let norm = apply(
            x,
            1,
            RATE,
            &Fx {
                normalize: true,
                gain: 60,
                ..Fx::default()
            },
        );
        let peak = norm.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.891).abs() < 0.005, "{peak}");
    }

    #[test]
    fn trims_quiet_ends() {
        let mut x = vec![0.0001f32; 4800]; // 100 ms of near silence
        x.extend(tone(0.2, 0.5));
        x.extend(vec![0.0001f32; 9600]);
        let out = apply(
            x,
            1,
            RATE,
            &Fx {
                trim_db: Some(-45),
                ..Fx::default()
            },
        );
        // 200 ms of tone plus 10 ms margins, give or take a frame.
        assert!((out.len() as i64 - 10_560).abs() < 50, "{}", out.len());
    }

    #[test]
    fn compressor_tames_loud_parts() {
        let mut x = tone(0.2, 0.05);
        x.extend(tone(0.2, 0.9));
        let out = apply(
            x,
            1,
            RATE,
            &Fx {
                compress: 80,
                ..Fx::default()
            },
        );
        let quiet = out[..9600].iter().fold(0f32, |m, v| m.max(v.abs()));
        let loud = out[14_400..].iter().fold(0f32, |m, v| m.max(v.abs()));
        // The gap between quiet and loud shrinks well below the original 18×.
        assert!(loud / quiet < 9.0, "{}", loud / quiet);
    }

    #[test]
    fn denoise_keeps_length_and_quiets_noise() {
        // White noise. RNNoise is trained on speech and takes off only part of
        // a flat hiss, so just check it gets clearly quieter.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..RATE as usize / 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                ((seed >> 16) as f32 / 32_768.0 - 1.0) * 0.1
            })
            .collect();
        let out = apply(
            noise.clone(),
            1,
            44_100,
            &Fx {
                denoise: true,
                ..Fx::default()
            },
        );
        assert_eq!(out.len(), noise.len());
        let energy = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
        assert!(energy(&out) < energy(&noise) * 0.8);
    }
}
