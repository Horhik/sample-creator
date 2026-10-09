//! Markup model and undo history. No GTK, no I/O beyond the `Store` trait.

pub mod history;

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
}

impl Region {
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

/// What the user can select in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    Marker(Id),
    Region(Id),
}
