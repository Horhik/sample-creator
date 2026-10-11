//! Markup model and undo history. No GTK, no I/O beyond the `Store` trait.

pub mod history;
pub mod parts;

pub use parts::Part;

/// Database id; 0 means "not stored yet".
pub type Id = i64;

/// Index into [`PALETTE`].
pub type ColorIdx = u8;

/// Fixed palette, keys 1–8. GNOME HIG colours, readable on light and dark.
pub const PALETTE: [(&str, [u8; 3]); 8] = [
    ("красный", [0xe0, 0x1b, 0x24]),
    ("оранжевый", [0xff, 0x78, 0x00]),
    ("жёлтый", [0xf6, 0xd3, 0x2d]),
    ("зелёный", [0x33, 0xd1, 0x7a]),
    ("бирюзовый", [0x2a, 0xc3, 0xde]),
    ("синий", [0x35, 0x84, 0xe4]),
    ("фиолетовый", [0x91, 0x41, 0xac]),
    ("розовый", [0xdc, 0x8a, 0xdd]),
];

/// Colour for markers imported from the recorder.
/// English colour names, in palette order.
const PALETTE_EN: [&str; 8] = [
    "red", "orange", "yellow", "green", "teal", "blue", "purple", "pink",
];

/// Name of palette colour `i` in the interface language.
pub fn color_name(i: usize) -> &'static str {
    let i = i % PALETTE.len();
    if crate::lang::is_russian() {
        PALETTE[i].0
    } else {
        PALETTE_EN[i]
    }
}

pub const RECORDER_MARK_COLOR: ColorIdx = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub id: Id,
    pub file: Id,
    /// Frames from the start of the file.
    pub pos: u64,
    pub name: String,
    pub color: ColorIdx,
    /// Imported from a recorder cue point rather than placed in the app.
    pub from_recorder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelMode {
    #[default]
    AsSource,
    Left,
    Right,
    /// Two mono files, `_L` and `_R`.
    SplitMono,
}

impl ChannelMode {
    pub fn to_db(self) -> i64 {
        match self {
            ChannelMode::AsSource => 0,
            ChannelMode::Left => 1,
            ChannelMode::Right => 2,
            ChannelMode::SplitMono => 3,
        }
    }

    pub fn from_db(v: i64) -> Self {
        match v {
            1 => ChannelMode::Left,
            2 => ChannelMode::Right,
            3 => ChannelMode::SplitMono,
            _ => ChannelMode::AsSource,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub id: Id,
    pub file: Id,
    /// `[start, end)` in frames.
    pub start: u64,
    pub end: u64,
    pub name: String,
    pub color: ColorIdx,
    pub export: bool,
    pub channels: ChannelMode,
    /// Playback speed in percent: 200 plays twice as fast (half as long).
    pub speed: u16,
    /// Change tempo only (time-stretch) instead of tempo and pitch together.
    pub keep_pitch: bool,
    /// Pieces of the region spliced together in this order, with fades. Empty
    /// means the whole region as is. See [`Region::effective_parts`].
    pub parts: Vec<Part>,
}

pub const SPEED_MIN: u16 = 25;
pub const SPEED_MAX: u16 = 400;
pub const SPEED_NORMAL: u16 = 100;

impl Region {
    /// Length in frames after splicing and the speed change.
    pub fn output_len(&self) -> u64 {
        let spliced = parts::spliced_len(&self.effective_parts());
        (spliced as f64 * f64::from(SPEED_NORMAL) / f64::from(self.speed.max(1))).round() as u64
    }

    /// The sound differs from a plain copy of `[start, end)`.
    pub fn is_processed(&self) -> bool {
        self.speed != SPEED_NORMAL || self.is_edited()
    }

    /// Cut, rearranged or faded.
    pub fn is_edited(&self) -> bool {
        self.effective_parts() != [Part::whole(self.start, self.end)]
    }

    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Everything marked up in one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markup {
    pub file: Id,
    /// Sorted by position.
    pub markers: Vec<Marker>,
    /// Sorted by start.
    pub regions: Vec<Region>,
}

impl Markup {
    pub fn marker(&self, id: Id) -> Option<&Marker> {
        self.markers.iter().find(|m| m.id == id)
    }

    pub fn region(&self, id: Id) -> Option<&Region> {
        self.regions.iter().find(|r| r.id == id)
    }

    pub fn sort(&mut self) {
        self.markers.sort_by_key(|m| (m.pos, m.id));
        self.regions.sort_by_key(|r| (r.start, r.end, r.id));
    }
}

/// Shift+click trimming: the edge on the clicked side of the centre of `[a, b)`
/// moves to `at`; never collapses below one frame.
pub fn trim_range(a: u64, b: u64, at: u64) -> (u64, u64) {
    if at >= a + (b - a) / 2 {
        (a, at.max(a + 1))
    } else {
        (at.min(b.saturating_sub(1)), b)
    }
}

/// What the user can select in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    Marker(Id),
    Region(Id),
}

#[cfg(test)]
mod tests {
    use super::trim_range;

    #[test]
    fn trim_moves_the_edge_on_the_clicked_side() {
        // Selection -10..10 shifted by 10: 0..20, centre 10.
        assert_eq!(trim_range(0, 20, 15), (0, 15)); // +5 → end
        assert_eq!(trim_range(0, 20, 11), (0, 11)); // +1 → end
        assert_eq!(trim_range(0, 20, 9), (9, 20)); // −1 → start
        assert_eq!(trim_range(0, 20, 30), (0, 30)); // past the end extends
        assert_eq!(trim_range(5, 6, 5), (5, 6)); // never empty
    }
}
