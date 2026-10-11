//! Speed change of interleaved audio.
//!
//! * Varispeed (`keep_pitch = false`): tape-style, tempo and pitch move together.
//!   Band-limited resampling with a Blackman-windowed sinc; when speeding up the
//!   cutoff drops with the speed so nothing aliases.
//! * Time-stretch (`keep_pitch = true`): WSOLA. Overlap-adds windowed grains taken
//!   at the new tempo, nudging each one to where it lines up best with the
//!   previous grain, so pitch stays put.

use std::f64::consts::PI;

/// Half-width of the sinc kernel in input samples (at unity cutoff).
const SINC_HALF: usize = 32;

/// `speed` is a factor: 2.0 plays twice as fast. Returns interleaved frames.
pub fn change_speed(
    input: &[f32],
    channels: usize,
    rate: u32,
    speed: f64,
    keep_pitch: bool,
) -> Vec<f32> {
    if (speed - 1.0).abs() < 1e-9 || input.is_empty() {
        return input.to_vec();
    }
    if keep_pitch {
        wsola(input, channels, rate, speed)
    } else {
        varispeed(input, channels, speed)
    }
}

fn blackman(x: f64) -> f64 {
    // x in [-1, 1]
    0.42 + 0.5 * (PI * x).cos() + 0.08 * (2.0 * PI * x).cos()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

fn varispeed(input: &[f32], channels: usize, speed: f64) -> Vec<f32> {
    let frames = input.len() / channels;
    let out_frames = (frames as f64 / speed).round() as usize;
    // Speeding up compresses the spectrum: low-pass at the new Nyquist.
    let cutoff = (1.0 / speed).min(1.0);
    // Keep the kernel the same number of zero crossings wide at any cutoff.
    let half = (SINC_HALF as f64 / cutoff).ceil() as i64;

    let mut out = vec![0f32; out_frames * channels];
    let mut acc = vec![0f64; channels];
    for n in 0..out_frames {
        let t = n as f64 * speed;
        let centre = t.floor() as i64;
        acc.iter_mut().for_each(|a| *a = 0.0);
        let mut weight_sum = 0.0;
        for k in (centre - half + 1)..=(centre + half) {
            if k < 0 || k as usize >= frames {
                continue;
            }
            let d = t - k as f64;
            let w = cutoff * sinc(cutoff * d) * blackman(d / half as f64);
            weight_sum += w;
            let frame = &input[k as usize * channels..(k as usize + 1) * channels];
            for (a, &s) in acc.iter_mut().zip(frame) {
                *a += w * f64::from(s);
            }
        }
        // Normalising keeps edges (where the kernel is cut off) at the right level.
        let norm = if weight_sum.abs() > 1e-6 {
            1.0 / weight_sum
        } else {
            0.0
        };
        for (o, a) in out[n * channels..(n + 1) * channels].iter_mut().zip(&acc) {
            *o = (a * norm) as f32;
        }
    }
    out
}

fn wsola(input: &[f32], channels: usize, rate: u32, speed: f64) -> Vec<f32> {
    let frames = input.len() / channels;
    let rate = f64::from(rate);
    // ~40 ms grains, 50% overlap, ±10 ms search: the usual choices for WSOLA.
    let grain = ((rate * 0.040) as usize).max(64) & !1;
    let syn_hop = grain / 2;
    let ana_hop = syn_hop as f64 * speed;
    let tolerance = (rate * 0.010) as i64;
    let out_frames = (frames as f64 / speed).round() as usize;

    let window: Vec<f64> = (0..grain)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / grain as f64).cos())
        .collect();
    let mono: Vec<f32> = input
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();

    let mut out = vec![0f64; (out_frames + grain) * channels];
    let mut norm = vec![0f64; out_frames + grain];
    let mut prev: Option<usize> = None;
    let mut m = 0usize;
    loop {
        let syn = m * syn_hop;
        if syn >= out_frames {
            break;
        }
        let nominal = (m as f64 * ana_hop).round() as i64;
        let pos = match prev {
            None => nominal.max(0) as usize,
            Some(p) => best_match(&mono, p + syn_hop, nominal, tolerance, grain),
        };
        if pos >= frames {
            break;
        }
        let len = grain.min(frames - pos);
        for i in 0..len {
            let w = window[i];
            norm[syn + i] += w;
            let src = &input[(pos + i) * channels..(pos + i + 1) * channels];
            let dst = &mut out[(syn + i) * channels..(syn + i + 1) * channels];
            for (d, &s) in dst.iter_mut().zip(src) {
                *d += w * f64::from(s);
            }
        }
        prev = Some(pos);
        m += 1;
    }

    let mut result = vec![0f32; out_frames * channels];
    for n in 0..out_frames {
        let g = if norm[n] > 1e-3 { 1.0 / norm[n] } else { 0.0 };
        for c in 0..channels {
            result[n * channels + c] = (out[n * channels + c] * g) as f32;
        }
    }
    result
}

/// Start position near `nominal` whose grain best continues the grain at `natural`.
/// Correlation runs on every 4th sample of half a grain: plenty to line up phases.
fn best_match(mono: &[f32], natural: usize, nominal: i64, tolerance: i64, grain: usize) -> usize {
    let frames = mono.len() as i64;
    let span = (grain / 2) as i64;
    let lo = (nominal - tolerance).max(0);
    let hi = (nominal + tolerance).min(frames - span - 1);
    if hi <= lo || natural as i64 + span >= frames {
        return nominal.clamp(0, frames.max(1) - 1) as usize;
    }
    let reference = &mono[natural..natural + span as usize];
    let mut best = (f64::MIN, nominal.clamp(lo, hi));
    for cand in lo..=hi {
        let seg = &mono[cand as usize..cand as usize + span as usize];
        let score: f64 = reference
            .iter()
            .zip(seg)
            .step_by(4)
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum();
        if score > best.0 {
            best = (score, cand);
        }
    }
    best.1 as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(freq: f64, secs: f64, channels: usize) -> Vec<f32> {
        let frames = (f64::from(RATE) * secs) as usize;
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * PI * freq * i as f64 / f64::from(RATE)).sin() as f32 * 0.5;
                std::iter::repeat_n(v, channels)
            })
            .collect()
    }

    /// Frequency estimate from upward zero crossings of channel 0, skipping the edges.
    fn freq(samples: &[f32], channels: usize) -> f64 {
        let mono: Vec<f32> = samples.iter().step_by(channels).copied().collect();
        let skip = mono.len() / 10;
        let body = &mono[skip..mono.len() - skip];
        let crossings = body
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        crossings as f64 / (body.len() as f64 / f64::from(RATE))
    }

    #[test]
    fn unity_speed_is_identity() {
        let x = sine(440.0, 0.1, 2);
        assert_eq!(change_speed(&x, 2, RATE, 1.0, false), x);
        assert_eq!(change_speed(&x, 2, RATE, 1.0, true), x);
    }

    #[test]
    fn varispeed_changes_length_and_pitch_together() {
        let x = sine(1000.0, 1.0, 2);
        let fast = change_speed(&x, 2, RATE, 2.0, false);
        assert_eq!(fast.len(), x.len() / 2);
        assert!((freq(&fast, 2) - 2000.0).abs() < 20.0, "{}", freq(&fast, 2));

        let slow = change_speed(&x, 2, RATE, 0.5, false);
        assert_eq!(slow.len(), x.len() * 2);
        assert!((freq(&slow, 2) - 500.0).abs() < 10.0);

        // Stereo channels stay separate.
        let left_only: Vec<f32> = x.chunks(2).flat_map(|f| [f[0], 0.0]).collect();
        let out = change_speed(&left_only, 2, RATE, 1.5, false);
        assert!(out.iter().skip(1).step_by(2).all(|&r| r.abs() < 1e-6));
    }

    #[test]
    fn varispeed_filters_what_would_alias() {
        // 18 kHz played twice as fast would land at 36 kHz, above Nyquist: it must
        // be removed, not folded back into the audible range.
        let x = sine(18_000.0, 0.5, 1);
        let fast = change_speed(&x, 1, RATE, 2.0, false);
        let body = &fast[fast.len() / 4..fast.len() * 3 / 4];
        let peak = body.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.02, "peak {peak}");
    }

    #[test]
    fn time_stretch_keeps_pitch() {
        let x = sine(440.0, 1.0, 2);
        for speed in [0.5, 0.8, 1.5, 2.0] {
            let out = change_speed(&x, 2, RATE, speed, true);
            let expected = (x.len() as f64 / speed).round() as usize;
            assert_eq!(out.len(), expected, "speed {speed}");
            let f = freq(&out, 2);
            assert!((f - 440.0).abs() < 440.0 * 0.03, "speed {speed}: {f} Hz");
        }
    }

    #[test]
    fn time_stretch_keeps_level() {
        let x = sine(440.0, 1.0, 1);
        let out = change_speed(&x, 1, RATE, 1.7, true);
        let body = &out[out.len() / 4..out.len() * 3 / 4];
        let peak = body.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.05, "peak {peak}");
    }
}
