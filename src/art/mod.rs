//! The first run's pictures: the PANDO wordmark, and the grove the name
//! comes from, both drawn in dithered blocks.
//!
//! Pando is one quaking aspen — some 47,000 white stems over a single root
//! system — and pando is named for it: one repository, every branch alive.
//!
//! Shaded by ordered dithering: a cell's intensity is compared with a 4×4
//! Bayer threshold and lands on one of `░▒▓█`, so gradients read as
//! texture rather than bands, in any font that has the block elements.
//!
//! Pure: the same size, frame and seed give the same cells, and only the
//! leaves and the wordmark's shine move between frames. The TUI and the
//! CLI colour the cells by their [`Material`], each from its own palette.

mod grove;
mod wordmark;

pub use grove::{
    GROVE_MIN_HEIGHT, GROVE_MIN_WIDTH, Grove, STEM_SPACING_MIN, grove, grove_of, stems_that_fit,
};
pub use wordmark::{COMPACT_WIDTH, WORDMARK_HEIGHT, WORDMARK_WIDTH, WordmarkSize, wordmark};

/// What a cell of a picture is, which decides its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Material {
    /// Nothing: the space around the picture.
    Sky,
    /// The leaves, which quake.
    Canopy,
    /// A white stem.
    Trunk,
    /// One of the dark marks on aspen bark.
    Eye,
    /// The ground line the stems stand on.
    Ground,
    /// The one root system under all of them.
    Root,
    /// The wordmark's letters.
    Letter,
    /// The wordmark's shadow.
    Shadow,
}

/// One cell: the character drawn and what it is part of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub material: Material,
}

impl Cell {
    pub(crate) const SKY: Cell = Cell {
        ch: ' ',
        material: Material::Sky,
    };

    pub(crate) fn new(ch: char, material: Material) -> Cell {
        Cell { ch, material }
    }
}

/// Who made pando, and where to find them: shown under the wordmark and
/// in the README, which a test holds to this.
pub const CREATOR: &str = "Mert Karadayi";
pub const CREATOR_URL: &str = "https://github.com/mertkaradayi";

/// The line under the wordmark.
pub fn credit() -> String {
    format!(
        "created by {CREATOR} · {}",
        CREATOR_URL.trim_start_matches("https://")
    )
}

/// The shades a dithered cell can take, from nothing to solid.
pub const RAMP: [char; 5] = [' ', '░', '▒', '▓', '█'];

/// The 4×4 Bayer matrix: the order in which cells of a block switch on as
/// the shade rises, spread so no two neighbours switch together.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// A seed from a name, so each project has a grove of its own and the
/// same grove every time.
pub fn seed_of(name: &str) -> u64 {
    // FNV-1a: stable across runs and builds, unlike the std hasher.
    name.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A picture as plain text, one line per row with trailing spaces kept:
/// for tests, and for anything that wants the shape without the colour.
pub fn to_text(cells: &[Vec<Cell>]) -> String {
    cells
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where each label goes under the stems of a grove `width` wide: the
/// stem it is written under, left-aligned there, for as many labels as
/// fit without two touching. Labels that do not fit are left out; a label
/// longer than the room to the next is not cut, it takes a later stem.
pub fn label_stems(stems: &[usize], labels: &[String], width: usize) -> Vec<(usize, String)> {
    let mut placed = Vec::new();
    let mut free_from = 0;
    let mut stems = stems.iter().copied();
    for label in labels {
        let length = label.chars().count();
        let Some(at) = stems.find(|&x| x >= free_from && x + length <= width) else {
            break;
        };
        placed.push((at, label.clone()));
        free_from = at + length + 2;
    }
    placed
}

/// The cells each of a row of labels gets of `room`, wanting `wants`
/// cells each, two cells kept between neighbours: each its whole where
/// all fit, and otherwise the short ones whole and what is left shared
/// evenly among the long, so no label has less than another for being
/// further right.
pub fn share_room(wants: &[usize], room: usize) -> Vec<usize> {
    let mut left = room.saturating_sub(2 * wants.len().saturating_sub(1));
    let mut given = vec![0; wants.len()];
    let mut order: Vec<usize> = (0..wants.len()).collect();
    order.sort_by_key(|&i| wants[i]);
    for (taken, i) in order.into_iter().enumerate() {
        let share = left / (wants.len() - taken);
        given[i] = wants[i].min(share);
        left -= given[i];
    }
    given
}

/// Where each label of a row starts, `widths[i]` cells for the stem at
/// `stems[i]`, in a row `width` wide: centred under its stem, and moved
/// as little as it takes to keep a cell from either edge and two from its
/// neighbours, in order. Labels whose widths [`share_room`] gave of
/// `width - 2` never touch.
pub fn spread_labels(stems: &[usize], widths: &[usize], width: usize) -> Vec<usize> {
    let (lo, hi) = (1, width.saturating_sub(1));
    let mut at: Vec<usize> = stems
        .iter()
        .zip(widths)
        .map(|(&x, &w)| x.saturating_sub(w / 2).min(hi.saturating_sub(w)).max(lo))
        .collect();
    for i in 1..at.len() {
        at[i] = at[i].max(at[i - 1] + widths[i - 1] + 2);
    }
    let mut limit = hi;
    for i in (0..at.len()).rev() {
        at[i] = at[i].min(limit.saturating_sub(widths[i]));
        limit = at[i].saturating_sub(2);
    }
    at
}

/// The dithered shade of `value` (0 to 1) at a cell: an index into
/// [`RAMP`]. The fraction between two shades is resolved against the
/// cell's Bayer threshold, so a smooth value reads as a texture.
pub(crate) fn dither(value: f64, x: usize, y: usize) -> usize {
    let scaled = value.clamp(0.0, 1.0) * (RAMP.len() - 1) as f64;
    let base = scaled.floor();
    let threshold = (f64::from(BAYER[y % 4][x % 4]) + 0.5) / 16.0;
    let level = base as usize + usize::from(scaled - base > threshold);
    level.min(RAMP.len() - 1)
}

/// A deterministic value in 0..1 for a lattice point.
pub(crate) fn hash01(seed: u64, x: u64, y: u64) -> f64 {
    let mut z =
        seed ^ x.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ y.wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise: the lattice's values, eased between.
pub(crate) fn noise(seed: u64, x: f64, y: f64) -> f64 {
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (ease(x - x0), ease(y - y0));
    let at = |dx: f64, dy: f64| hash01(seed, (x0 + dx) as i64 as u64, (y0 + dy) as i64 as u64);
    let top = at(0.0, 0.0) + (at(1.0, 0.0) - at(0.0, 0.0)) * tx;
    let bottom = at(0.0, 1.0) + (at(1.0, 1.0) - at(0.0, 1.0)) * tx;
    top + (bottom - top) * ty
}

fn ease(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests;
