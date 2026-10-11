//! Renders a region's parts (see [`crate::core::Part`]): each part gets its
//! fades and is mixed in at its place on the output timeline, so overlapping
//! parts sound together. Equal-power curves, so a crossfade between unrelated
//! sounds keeps its level.

use std::f64::consts::FRAC_PI_2;

use crate::core::Part;
use crate::core::parts::{part_fades, positions, spliced_len};

/// `samples`: the whole region, interleaved, its first frame at `region_start`.
pub fn splice(samples: &[f32], channels: usize, region_start: u64, parts: &[Part]) -> Vec<f32> {
    let frames = (samples.len() / channels) as u64;
    let mut out = vec![0f32; spliced_len(parts) as usize * channels];
    for (part, at) in parts.iter().zip(positions(parts)) {
        let (fi, fo) = part_fades(part);
        let len = part.len();
        let first = part.start.saturating_sub(region_start).min(frames);
        for n in 0..len {
            let src = first + n;
            if src >= frames {
                break;
            }
            let mut gain = 1.0;
            if n < fi {
                gain *= ((n as f64 + 0.5) / fi as f64 * FRAC_PI_2).sin();
            }
            let to_end = len - n;
            if to_end <= fo {
                gain *= ((to_end as f64 - 0.5) / fo as f64 * FRAC_PI_2).sin();
            }
            let dst = ((at + n) as usize) * channels;
            let src = src as usize * channels;
            for c in 0..channels {
                out[dst + c] += (f64::from(samples[src + c]) * gain) as f32;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|i| i as f32).collect()
    }

    #[test]
    fn plain_cut_and_join_copies_samples() {
        let x = ramp(1000);
        let parts = [Part::whole(100, 300), Part::whole(700, 800)];
        let out = splice(&x, 1, 0, &parts);
        assert_eq!(out.len(), 300);
        assert_eq!(out[0], 100.0);
        assert_eq!(out[199], 299.0);
        assert_eq!(out[200], 700.0);
    }

    #[test]
    fn region_offset_and_stereo() {
        let x: Vec<f32> = (0..200).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let out = splice(&x, 2, 1000, &[Part::whole(1050, 1060)]);
        assert_eq!(&out[..4], &[50.0, -50.0, 51.0, -51.0]);
    }

    #[test]
    fn crossfade_keeps_constant_level() {
        // Two parts of a constant signal crossfaded: equal power means the
        // squared gains sum to one, so the level dips by at most 3 dB.
        let x = vec![1.0f32; 2000];
        let parts = [
            Part {
                fade_out: 200,
                ..Part::whole(0, 1000)
            },
            Part {
                fade_in: 200,
                overlap: 200,
                ..Part::whole(1000, 2000)
            },
        ];
        let out = splice(&x, 1, 0, &parts);
        assert_eq!(out.len(), 1800);
        let mid = &out[800..1000];
        assert!(mid.iter().all(|&v| (0.99..=1.42).contains(&v)), "{mid:?}");
        assert!((out[0] - 1.0).abs() < 1e-6 && (out[1799] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pause_between_parts_is_silent() {
        let x = vec![1.0f32; 100];
        let parts = [
            Part::whole(0, 50),
            Part {
                overlap: -20,
                ..Part::whole(50, 100)
            },
        ];
        let out = splice(&x, 1, 0, &parts);
        assert_eq!(out.len(), 120);
        assert!(out[50..70].iter().all(|&v| v == 0.0));
        assert_eq!(out[70], 1.0);
    }

    #[test]
    fn edge_fades_go_to_silence() {
        let x = vec![1.0f32; 1000];
        let parts = [Part {
            fade_in: 100,
            fade_out: 100,
            ..Part::whole(0, 1000)
        }];
        let out = splice(&x, 1, 0, &parts);
        assert!(out[0] < 0.02 && out[999] < 0.02);
        assert!((out[500] - 1.0).abs() < 1e-6);
    }
}
