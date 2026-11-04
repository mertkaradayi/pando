//! The grove: Pando itself, drawn in dithered blocks.
//!
//! A row of slender aspen stems, with the dark "eyes" their bark has,
//! under a canopy of leaves that quakes from frame to frame, standing on
//! the ground over one root system: a tap root under every stem, and one
//! root line that joins them all, running off both edges of the picture
//! the way Pando's runs on under the hill. Each stem is where a branch
//! lives; the renderers write the worktrees' names under them.

use super::{Cell, Material, dither, hash01, noise};

/// The smallest grove worth drawing: below it the stems and the roots
/// have no room to read as a grove, so nothing is drawn at all.
pub const GROVE_MIN_WIDTH: usize = 24;
pub const GROVE_MIN_HEIGHT: usize = 7;

/// Rows under the ground: the tap roots, and the root line.
const ROOT_ROWS: usize = 2;

/// A drawn grove: its cells, rows top to bottom, and the column each stem
/// stands in, so a renderer can name them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grove {
    pub cells: Vec<Vec<Cell>>,
    pub stems: Vec<usize>,
}

impl Grove {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// The grove at `width` × `height` cells on animation frame `frame`.
/// Empty below [`GROVE_MIN_WIDTH`] × [`GROVE_MIN_HEIGHT`].
pub fn grove(width: usize, height: usize, frame: u64, seed: u64) -> Grove {
    if width < GROVE_MIN_WIDTH || height < GROVE_MIN_HEIGHT {
        return Grove {
            cells: Vec::new(),
            stems: Vec::new(),
        };
    }
    let scene = Scene::new(width, height, seed);
    Grove {
        cells: (0..height)
            .map(|y| (0..width).map(|x| scene.cell(x, y, frame)).collect())
            .collect(),
        stems: scene.trunks.iter().map(|t| t.x).collect(),
    }
}

/// The fewest cells between two stems of [`grove_of`]: closer, and the
/// crowns run into one hedge and the names under them into each other.
pub const STEM_SPACING_MIN: usize = 5;

/// A grove with one stem for each entry of `leafed`, evenly spaced across
/// `width`, in leaf where its entry is true and bare where it is false:
/// the projects on a machine, the ones with something up in leaf. Empty
/// below [`GROVE_MIN_WIDTH`] × [`GROVE_MIN_HEIGHT`]; the caller keeps the
/// stems [`STEM_SPACING_MIN`] apart, see [`stems_that_fit`].
pub fn grove_of(width: usize, height: usize, frame: u64, seed: u64, leafed: &[bool]) -> Grove {
    if width < GROVE_MIN_WIDTH || height < GROVE_MIN_HEIGHT {
        return Grove {
            cells: Vec::new(),
            stems: Vec::new(),
        };
    }
    let scene = Scene::with_stems(width, height, seed, leafed);
    Grove {
        cells: (0..height)
            .map(|y| (0..width).map(|x| scene.cell(x, y, frame)).collect())
            .collect(),
        stems: scene.trunks.iter().map(|t| t.x).collect(),
    }
}

/// How many evenly spaced stems a grove `width` wide holds.
pub fn stems_that_fit(width: usize) -> usize {
    width / STEM_SPACING_MIN
}

/// One stem: where it stands, and where its crown is.
struct Trunk {
    x: usize,
    /// In leaf, or a bare stem whose crown is not drawn.
    leafed: bool,
    /// The row its crown is centred on; the stem shows from there down.
    crown_y: f64,
    /// How far its crown reaches either side, and up and down.
    crown_rx: f64,
    crown_ry: f64,
}

struct Scene {
    seed: u64,
    width: usize,
    trunks: Vec<Trunk>,
    /// The row the stems stand on.
    ground: usize,
    /// The lowest row a crown reaches.
    canopy_floor: usize,
}

impl Scene {
    fn new(width: usize, height: usize, seed: u64) -> Scene {
        let ground = height - ROOT_ROWS - 1;
        let canopy_floor = ((ground as f64) * 0.62).round().max(2.0) as usize;
        let mut rng = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
        let mut trunks = Vec::new();
        // Stems five to eight cells apart, starting a little way in and
        // stopping before the edge, so the crowns are not cut in half.
        let mut x = 2 + (rng.next() % 3) as usize;
        while x + 3 < width {
            // Crowns of their own, at heights of their own: a grove, not
            // a hedge.
            let crown_y = 1.2 + rng.unit() * (canopy_floor as f64 * 0.55);
            trunks.push(Trunk {
                x,
                leafed: true,
                crown_y,
                crown_rx: 2.2 + rng.unit() * 1.6,
                crown_ry: (canopy_floor as f64) * (0.38 + rng.unit() * 0.22),
            });
            x += 5 + (rng.next() % 4) as usize;
        }
        Scene {
            seed,
            width,
            trunks,
            ground,
            canopy_floor,
        }
    }

    /// One stem per entry of `leafed`, each in the middle of an equal
    /// share of the width.
    fn with_stems(width: usize, height: usize, seed: u64, leafed: &[bool]) -> Scene {
        let ground = height - ROOT_ROWS - 1;
        let canopy_floor = ((ground as f64) * 0.62).round().max(2.0) as usize;
        let mut rng = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
        let share = width / leafed.len().max(1);
        let trunks = leafed
            .iter()
            .enumerate()
            .map(|(i, &leafed)| {
                let crown_y = 1.2 + rng.unit() * (canopy_floor as f64 * 0.55);
                // A crown no wider than its share, so neighbours stay two.
                let reach = ((share as f64) / 2.0 - 0.5).max(1.2);
                Trunk {
                    x: i * share + share / 2,
                    leafed,
                    crown_y,
                    crown_rx: (2.2 + rng.unit() * 1.6).min(reach),
                    crown_ry: (canopy_floor as f64) * (0.38 + rng.unit() * 0.22),
                }
            })
            .collect();
        Scene {
            seed,
            width,
            trunks,
            ground,
            canopy_floor,
        }
    }

    fn cell(&self, x: usize, y: usize, frame: u64) -> Cell {
        if y > self.ground {
            return self.root(x, y - self.ground);
        }
        if y == self.ground {
            return self.ground_cell(x);
        }
        let stem = self.trunks.iter().find(|t| t.x == x);
        // A stem shows below its crown's centre, and through the canopy
        // where the leaves are thin — judged on the still canopy, so a
        // stem never flickers with the quake.
        if let Some(t) = stem
            && (y as f64) >= t.crown_y
            && (self.canopy(x, y, None) < 0.55 || y >= self.canopy_floor)
        {
            // An eye every few rows, in the same places every frame.
            return match hash01(self.seed, x as u64, y as u64 + 7) < 0.22 {
                true => Cell::new('▓', Material::Eye),
                false => Cell::new('█', Material::Trunk),
            };
        }
        match dither(self.canopy(x, y, Some(frame)), x, y) {
            0 => Cell::SKY,
            level => Cell::new(super::RAMP[level], Material::Canopy),
        }
    }

    /// How dense the leaves are at a cell, 0 to 1: quaking on `frame`, or
    /// still with none.
    fn canopy(&self, x: usize, y: usize, frame: Option<u64>) -> f64 {
        if y > self.canopy_floor {
            return 0.0;
        }
        let (fx, fy) = (x as f64, y as f64);
        let crowns = self
            .trunks
            .iter()
            .filter(|t| t.leafed)
            .map(|t| {
                let dx = (fx - t.x as f64) / t.crown_rx;
                let dy = (fy - t.crown_y) / t.crown_ry;
                1.0 - (dx * dx + dy * dy)
            })
            .fold(0.0_f64, f64::max);
        if crowns <= 0.0 {
            return 0.0;
        }
        // Clumps of leaves rather than smooth domes.
        let clumps = (noise(self.seed, fx * 0.45, fy * 0.8) - 0.5) * 0.55;
        // The quake: each cell trembles on its own phase, a little.
        let phase = hash01(self.seed, x as u64, y as u64) * std::f64::consts::TAU;
        let quake = frame.map_or(0.0, |frame| 0.11 * ((frame as f64) * 1.3 + phase).sin());
        (crowns * 1.25 + clumps + quake).clamp(0.0, 1.0)
    }

    fn ground_cell(&self, x: usize) -> Cell {
        if self.trunks.iter().any(|t| t.x == x) {
            return Cell::new('█', Material::Trunk);
        }
        let shade = 0.22 + (noise(self.seed ^ 1, x as f64 * 0.5, 0.0) - 0.5) * 0.3;
        Cell::new(
            super::RAMP[dither(shade, x, self.ground).max(1)],
            Material::Ground,
        )
    }

    /// The root system, `depth` rows under the ground: a tap root down
    /// from every stem, then one line joining them all and running on
    /// past both edges — one organism, however many stems.
    fn root(&self, x: usize, depth: usize) -> Cell {
        let stem = self.trunks.iter().any(|t| t.x == x);
        match depth {
            1 if stem => Cell::new('┃', Material::Root),
            1 => Cell::SKY,
            _ if stem => Cell::new('┻', Material::Root),
            _ if x == 0 => Cell::new('╺', Material::Root),
            _ if x + 1 == self.width => Cell::new('╸', Material::Root),
            _ => Cell::new('━', Material::Root),
        }
    }
}

/// splitmix64: small, fast, and the same everywhere.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}
