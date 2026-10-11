//! Cutting a region into parts and splicing them back.
//!
//! A region's sound is its parts laid end to end in list order on an output
//! timeline. Each part may slide back over the previous one (`overlap > 0`,
//! the two sound together) or leave a pause after it (`overlap < 0`). Fades
//! are the part's own; a crossfade is an overlap with matching fades. All
//! lengths are frames.

use super::Region;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    /// `[start, end)` in frames of the source file, inside the region.
    pub start: u64,
    pub end: u64,
    pub fade_in: u64,
    pub fade_out: u64,
    /// Frames this part starts before the previous one ends; negative is a
    /// pause between them. Ignored for the first part.
    pub overlap: i64,
}

impl Part {
    pub fn whole(start: u64, end: u64) -> Self {
        Part {
            start,
            end,
            fade_in: 0,
            fade_out: 0,
            overlap: 0,
        }
    }

    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fades never overlap each other inside one part.
    fn fades(&self) -> (u64, u64) {
        let len = self.len();
        let (fi, fo) = (self.fade_in.min(len), self.fade_out.min(len));
        if fi + fo <= len {
            (fi, fo)
        } else {
            // Scale both down to fit.
            let fi2 = (fi as u128 * len as u128 / (fi + fo) as u128) as u64;
            (fi2, len - fi2)
        }
    }
}

/// Gains for one part: fade-in and fade-out lengths after fitting.
pub fn part_fades(part: &Part) -> (u64, u64) {
    part.fades()
}

/// Where each part starts on the output timeline.
pub fn positions(parts: &[Part]) -> Vec<u64> {
    let mut out = Vec::with_capacity(parts.len());
    let mut end = 0i64;
    for (i, p) in parts.iter().enumerate() {
        let start = if i == 0 { 0 } else { (end - p.overlap).max(0) };
        out.push(start as u64);
        end = start + p.len() as i64;
    }
    out
}

/// Length of the spliced sound.
pub fn spliced_len(parts: &[Part]) -> u64 {
    positions(parts)
        .iter()
        .zip(parts)
        .map(|(pos, p)| pos + p.len())
        .max()
        .unwrap_or(0)
}

/// The part heard at output frame `t` and the matching source frame. Where
/// parts overlap, the later one wins.
pub fn output_to_source(parts: &[Part], t: u64) -> Option<(usize, u64)> {
    positions(parts)
        .iter()
        .zip(parts)
        .enumerate()
        .rev()
        .find(|(_, (pos, p))| **pos <= t && t < **pos + p.len())
        .map(|(i, (pos, p))| (i, p.start + (t - pos)))
}

impl Region {
    /// The parts as they sound: clipped to the region (its edges may have moved
    /// since the cut), empty ones dropped, overlaps kept within the neighbours,
    /// the whole region when none are left.
    pub fn effective_parts(&self) -> Vec<Part> {
        let mut parts: Vec<Part> = self
            .parts
            .iter()
            .map(|p| Part {
                start: p.start.clamp(self.start, self.end),
                end: p.end.clamp(self.start, self.end),
                ..*p
            })
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return vec![Part::whole(self.start, self.end)];
        }
        parts[0].overlap = 0;
        for i in 1..parts.len() {
            // A part never reaches past the start of the previous one, nor ends
            // before it does: the order of the list is the order of the sound.
            let limit = parts[i - 1].len().min(parts[i].len()) as i64;
            parts[i].overlap = parts[i].overlap.min(limit);
        }
        parts
    }

    /// Cuts part `index` at source frame `at`. The halves keep the outer fades
    /// and meet without overlap. Returns false when `at` is not inside it.
    pub fn split_part(&mut self, index: usize, at: u64) -> bool {
        let mut parts = self.effective_parts();
        let Some(&p) = parts.get(index).filter(|p| p.start < at && at < p.end) else {
            return false;
        };
        parts[index] = Part {
            end: at,
            fade_out: 0,
            ..p
        };
        parts.insert(
            index + 1,
            Part {
                start: at,
                fade_in: 0,
                overlap: 0,
                ..p
            },
        );
        self.parts = parts;
        true
    }

    /// Cuts the first part that contains source frame `at`.
    pub fn split_at(&mut self, at: u64) -> bool {
        let parts = self.effective_parts();
        match parts.iter().position(|p| p.start < at && at < p.end) {
            Some(i) => self.split_part(i, at),
            None => false,
        }
    }

    /// Drops a part; the ones after it close the gap. The last one cannot go.
    pub fn remove_part(&mut self, index: usize) -> bool {
        let mut parts = self.effective_parts();
        if parts.len() < 2 || index >= parts.len() {
            return false;
        }
        parts.remove(index);
        self.parts = parts;
        true
    }

    /// Swaps a part with its neighbour: `up` towards the beginning.
    pub fn move_part(&mut self, index: usize, up: bool) -> bool {
        let mut parts = self.effective_parts();
        let other = if up {
            index.checked_sub(1)
        } else {
            Some(index + 1)
        };
        let Some(other) = other.filter(|&o| o < parts.len() && index < parts.len()) else {
            return false;
        };
        parts.swap(index, other);
        self.parts = parts;
        true
    }

    pub fn set_fades(&mut self, index: usize, fade_in: u64, fade_out: u64) -> bool {
        let mut parts = self.effective_parts();
        let Some(p) = parts.get_mut(index) else {
            return false;
        };
        p.fade_in = fade_in;
        p.fade_out = fade_out;
        self.parts = parts;
        true
    }

    /// Slides a part over the previous one (or away from it, leaving a pause).
    pub fn set_overlap(&mut self, index: usize, overlap: i64) -> bool {
        let mut parts = self.effective_parts();
        if index == 0 || index >= parts.len() {
            return false;
        }
        parts[index].overlap = overlap;
        self.parts = parts;
        true
    }

    /// Moves a part's edges, keeping it inside the region and at least one
    /// frame long. Its sound stays where it was on the output timeline: moving
    /// the start later shifts the part's output start by the same amount.
    pub fn set_part_bounds(&mut self, index: usize, start: u64, end: u64) -> bool {
        let (lo, hi) = (self.start, self.end);
        let mut parts = self.effective_parts();
        let Some(p) = parts.get_mut(index) else {
            return false;
        };
        let start = start.clamp(lo, hi.saturating_sub(1));
        let end = end.clamp(start + 1, hi);
        p.overlap -= start as i64 - p.start as i64;
        p.start = start;
        p.end = end;
        self.parts = parts;
        true
    }

    /// Back to the whole region, no fades.
    pub fn reset_parts(&mut self) {
        self.parts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ChannelMode;

    fn region(start: u64, end: u64) -> Region {
        Region {
            id: 1,
            file: 1,
            start,
            end,
            name: String::new(),
            color: 0,
            export: true,
            channels: ChannelMode::AsSource,
            speed: 100,
            keep_pitch: false,
            parts: Vec::new(),
        }
    }

    #[test]
    fn untouched_region_is_one_part() {
        let r = region(100, 1100);
        assert_eq!(r.effective_parts(), [Part::whole(100, 1100)]);
        assert!(!r.is_edited());
        assert_eq!(r.output_len(), 1000);
    }

    #[test]
    fn cut_remove_and_splice() {
        let mut r = region(0, 1000);
        assert!(r.split_at(300));
        assert!(r.split_at(600));
        assert!(!r.split_at(600), "a cut point is not inside a part");
        assert_eq!(r.effective_parts().len(), 3);
        // Cutting alone changes nothing audible.
        assert_eq!(r.output_len(), 1000);
        assert!(r.remove_part(1));
        assert_eq!(r.output_len(), 700);
        assert!(r.is_edited());
        assert!(r.move_part(1, true));
        assert_eq!(r.effective_parts()[0].start, 600);
        assert!(!r.move_part(0, true));
    }

    #[test]
    fn overlap_and_pause_move_the_following_parts() {
        let mut r = region(0, 1000);
        r.split_at(400);
        r.split_at(700);
        assert!(r.set_overlap(1, 100));
        let parts = r.effective_parts();
        assert_eq!(positions(&parts), [0, 300, 600]);
        assert_eq!(r.output_len(), 900);
        // A pause instead.
        r.set_overlap(1, -50);
        assert_eq!(positions(&r.effective_parts()), [0, 450, 750]);
        assert_eq!(r.output_len(), 1050);
        // Never past the previous part's start or shorter neighbour.
        r.set_overlap(2, 10_000);
        assert_eq!(r.effective_parts()[2].overlap, 300);
        assert!(!r.set_overlap(0, 5));
    }

    #[test]
    fn output_frames_map_back_to_the_later_part() {
        let mut r = region(0, 1000);
        r.split_at(500);
        r.set_overlap(1, 100);
        let parts = r.effective_parts();
        assert_eq!(output_to_source(&parts, 10), Some((0, 10)));
        // 450 is in both; the later part (starting at 400, source 500) wins.
        assert_eq!(output_to_source(&parts, 450), Some((1, 550)));
        assert_eq!(output_to_source(&parts, 899), Some((1, 999)));
        assert_eq!(output_to_source(&parts, 900), None);
    }

    #[test]
    fn split_part_by_index_even_when_sources_repeat() {
        let mut r = region(0, 1000);
        r.split_at(500);
        r.set_part_bounds(1, 200, 1000); // now both parts contain source 300
        assert!(r.split_part(1, 300));
        let parts = r.effective_parts();
        assert_eq!(parts.len(), 3);
        assert_eq!((parts[1].start, parts[1].end), (200, 300));
        assert_eq!((parts[0].start, parts[0].end), (0, 500));
    }

    #[test]
    fn trimming_the_start_keeps_the_sound_in_place() {
        let mut r = region(0, 1000);
        r.split_at(500);
        let before = positions(&r.effective_parts())[1] + 100;
        assert!(r.set_part_bounds(1, 600, 1000));
        let parts = r.effective_parts();
        assert_eq!(parts[1].overlap, -100);
        assert_eq!(positions(&parts)[1], before);
    }

    #[test]
    fn fades_fit_inside_short_parts() {
        let p = Part {
            fade_in: 80,
            fade_out: 80,
            ..Part::whole(0, 100)
        };
        let (fi, fo) = part_fades(&p);
        assert_eq!(fi + fo, 100);
    }

    #[test]
    fn part_bounds_stay_inside_the_region() {
        let mut r = region(100, 1000);
        r.split_at(500);
        assert!(r.set_part_bounds(0, 50, 450));
        assert_eq!(
            (r.effective_parts()[0].start, r.effective_parts()[0].end),
            (100, 450)
        );
        assert!(r.set_part_bounds(1, 900, 800));
        let p = r.effective_parts()[1];
        assert_eq!((p.start, p.end), (900, 901));
    }

    #[test]
    fn moved_edges_clip_parts() {
        let mut r = region(0, 1000);
        r.split_at(500);
        r.end = 400;
        assert_eq!(r.effective_parts(), [Part::whole(0, 400)]);
    }
}
